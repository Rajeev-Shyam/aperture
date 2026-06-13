//! The R1 projection check + the R3 degrade ladder (doc 04 §6, doc 12 §4).
//!
//! Invariant (1): the 8 GB VRAM ceiling. Every load/job is admitted **iff**
//! ```text
//! projected = weights + mmproj + kv_est(ctx_tokens) + img_act(n_images) + framework
//! projected <= 7.2 GB        // 0.8 GB margin under the 8 GB ceiling (doc 04 R1)
//! ```
//! On refusal the caller is hung off the R3 degrade ladder (doc 04 §6) rather
//! than left guessing — the projection rides back in
//! [`aperture_contracts::JobError::BudgetRefused`].

use crate::vram_table::{ModelId, VramTable};

/// The hard projection ceiling: 0.8 GB under the 8 GB GPU (doc 04 R1, doc 12 §4).
pub const PROJECTION_CEILING_GB: f32 = 7.2;

/// The R3 degrade ladder, **in order** (doc 04 R3, doc 12 §4). The enforcer
/// proposes the next rung when a projection fails; the scheduler applies it and
/// re-projects until something fits or it reaches [`Refuse`](DegradeRung::Refuse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeRung {
    /// 7B -> 3B (doc 04 R3 step 1).
    DropTo3b,
    /// Shrink context: 8K -> 4K -> 2K (doc 04 R3 step 2). Carries the next ctx.
    ShrinkCtx { next_ctx_tokens: u32 },
    /// Drop the image -> OCR-text-only prompt (doc 04 R3 step 3, the R2 escape).
    DropImage,
    /// Queue the job behind the mutex (doc 04 R3 step 4).
    Queue,
    /// Refuse with a UI notice (doc 04 R3 step 5, the terminal rung).
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

    /// Project the VRAM for one load request (doc 04 R1, doc 12 §4):
    /// `weights + mmproj + kv_est(ctx) + img_act(n_images) + framework`.
    pub fn project(&self, _req: LoadRequest) -> Projection {
        // TODO(M5:) compute against measured rows once the M5 harness has run:
        //   let p = self.table.params(req.model)?;
        //   let kv = if p.kv_ref_ctx_tokens > 0 {
        //       p.kv_ref_gb * (req.ctx_tokens as f32 / p.kv_ref_ctx_tokens as f32)
        //   } else { 0.0 };
        //   let img = p.img_act_per_image_gb * req.n_images as f32;
        //   let projected = p.weights_gb + p.mmproj_gb + kv + img + self.table.framework_gb;
        //   Projection { projected_gb: projected, admit: projected <= PROJECTION_CEILING_GB }
        // An unmeasured row (post-OOM) must conservative-cap, not optimistic-pass (doc 12 §7).
        todo!("M5: R1 projection = weights+mmproj+kv_est(ctx)+img_act(n)+framework <= 7.2 GB")
    }

    /// Given a failed [`Projection`] for `req`, return the next R3 rung to try
    /// (doc 04 R3, doc 12 §4). Returns `Refuse` once the ladder is exhausted.
    pub fn next_rung(&self, _req: LoadRequest, _failed: Projection) -> DegradeRung {
        // TODO(M5:) walk the ladder in doc 04 R3 order:
        //   Vlm7b            -> DropTo3b
        //   ctx 8K           -> ShrinkCtx{4K};  4K -> ShrinkCtx{2K}
        //   n_images == 1    -> DropImage
        //   else             -> Queue, then Refuse
        todo!("M5: R3 degrade ladder: 7B->3B -> ctx 8K->4K->2K -> drop image -> queue -> refuse")
    }
}
