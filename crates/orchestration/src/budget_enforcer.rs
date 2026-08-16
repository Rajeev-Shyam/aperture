//! The R1 projection check + the R3 degrade ladder (doc 04 §6, doc 12 §4).
//!
//! Invariant (1): the VRAM ceiling. Every load/job is admitted **iff**
//! ```text
//! projected = active(weights + mmproj + kv_est(ctx_tokens) + img_act(n_images))
//!           + framework + co_resident_weights      // ADR-030: co-resident weights ARE counted
//! projected <= ceiling_gb    // detected total VRAM − 1.0 GB headroom (decision #43);
//!                            // falls back to the 7.0 GB constant (doc 04 R1, 8 GB GPU)
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

/// The **fallback** projection ceiling: 1.0 GB under the 8 GB GPU (doc 04 R1,
/// ADR-030; amended from R1's 7.2 — the projection now counts co-resident
/// weights). ADR-030 documented 7.0 as locked; **decision #43 reversed that**:
/// the production ceiling auto-scales to the installed GPU (see
/// [`startup_projection_ceiling_gb`]), and this constant remains only as the
/// detection-failure fallback (and the deterministic default for tests).
pub const PROJECTION_CEILING_GB: f32 = 7.0;

/// OS/compositor VRAM headroom subtracted from the detected total (decision
/// #43) — the same 1.0 GB margin doc 04 R1 kept under the 8 GB GPU.
const VRAM_HEADROOM_GB: f32 = 1.0;
/// Sane clamp bounds for a *detected* ceiling (decision #43). The floor guards
/// an absurd parse, not usability — a 2 GB budget honestly refuses every model,
/// which beats projecting against garbage. The cap covers the largest consumer
/// card (32 GB) minus headroom.
const DETECTED_CEILING_MIN_GB: f32 = 2.0;
const DETECTED_CEILING_MAX_GB: f32 = 31.0;

/// Derive the projection ceiling from a detected total-VRAM figure (MiB):
/// total − [`VRAM_HEADROOM_GB`], clamped to the sane bounds above (decision
/// #43). Pure so the derivation is testable without a GPU; an 8192 MiB card
/// yields exactly the historical 7.0.
fn ceiling_from_total_mib(total_mib: u64) -> f32 {
    (total_mib as f32 / 1024.0 - VRAM_HEADROOM_GB)
        .clamp(DETECTED_CEILING_MIN_GB, DETECTED_CEILING_MAX_GB)
}

/// Parse `nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits`
/// output: one MiB integer per line, one line per GPU. The first line is GPU 0
/// — the device llama.cpp/CTranslate2 load onto by default.
fn parse_total_mib(stdout: &str) -> Option<u64> {
    stdout.lines().next()?.trim().parse::<u64>().ok()
}

/// Query the installed GPU's total VRAM (MiB) via `nvidia-smi` — the same
/// measurement precedent as the SC6/M5 gate harnesses. A local measurement
/// `Command` in a lint-emitters SANCTIONED crate (doc 13 §2), never a socket.
/// **Startup only** (decision #43): callers cache the result; this must never
/// sit on a per-request path.
fn detect_total_vram_mib() -> Option<u64> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"]);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW — mirror the sidecar spawn: nvidia-smi is a console
        // binary and would otherwise flash a terminal at app launch.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_total_mib(&String::from_utf8_lossy(&out.stdout))
}

/// The process-wide projection ceiling (decision #43): detected once at
/// startup (`OnceLock` — exactly one `nvidia-smi` spawn per process, never on
/// a request path), **fixed thereafter**, falling back to
/// [`PROJECTION_CEILING_GB`] when detection fails (no NVIDIA GPU, no
/// `nvidia-smi` on PATH, unparseable output).
pub fn startup_projection_ceiling_gb() -> f32 {
    static CEILING: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *CEILING.get_or_init(|| match detect_total_vram_mib() {
        Some(mib) => {
            let ceiling = ceiling_from_total_mib(mib);
            tracing::info!(
                total_mib = mib,
                ceiling_gb = ceiling,
                "VRAM ceiling auto-scaled to the installed GPU (decision #43)"
            );
            ceiling
        }
        None => {
            tracing::warn!(
                "VRAM detection failed; falling back to the {PROJECTION_CEILING_GB} GB ceiling (decision #43)"
            );
            PROJECTION_CEILING_GB
        }
    })
}

/// Context-shrink ladder rungs (doc 04 R3 step 2): 8K -> 4K -> 2K.
const CTX_SHRINK_HIGH: u32 = 4096;
const CTX_SHRINK_LOW: u32 = 2048;

/// The R3 degrade ladder, **in order** (doc 04 R3, doc 12 §4). The enforcer
/// proposes the next rung when a projection fails; [`BudgetEnforcer::admit`]
/// applies it and re-projects until something fits or it reaches a terminal rung.
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
    /// Terminal for the scheduler: a VLM job with no image is the caller's
    /// OCR-only degrade (doc 06 §6), so `admit` returns `Refused` here.
    DropImage,
    /// Queue the job behind the mutex (doc 04 R3 step 5). Reserved — the single
    /// permit already serializes contention, so `admit` treats it as terminal.
    Queue,
    /// Refuse with a UI notice (doc 04 R3 step 6, the terminal rung).
    Refuse,
}

/// What the enforcer was asked to admit: a concrete loadout for one job.
#[derive(Debug, Clone, Copy, PartialEq)]
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

/// The outcome of walking the R3 ladder for one request (doc 04 §6).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Admission {
    /// Admit at this (possibly-degraded) request. The scheduler applies
    /// `unload_stt` (kill the co-resident STT) before loading `plan.model`, then
    /// runs the job. `degraded` is `true` if any R3 rung was applied.
    Admit {
        plan: LoadRequest,
        unload_stt: bool,
        projected_gb: f32,
        degraded: bool,
    },
    /// Even the terminal rung didn't fit; the caller degrades (OCR-only, doc 06
    /// §6). `projection_gb` rides back in `JobError::BudgetRefused`.
    Refused { projection_gb: f32 },
}

/// Owns the [`VramTable`] and runs the R1 check (doc 04 §6, doc 12 §4).
#[derive(Debug)]
pub struct BudgetEnforcer {
    table: VramTable,
    /// The projection ceiling this enforcer admits under. Set once at
    /// construction (decision #43: fixed after startup — no setter).
    ceiling_gb: f32,
}

fn is_stt(model: ModelId) -> bool {
    matches!(
        model,
        ModelId::FasterWhisperSmall | ModelId::WhisperDistilLargeV3Int8
    )
}

impl BudgetEnforcer {
    /// Construct over a (seeded or measured) VRAM table, at the deterministic
    /// [`PROJECTION_CEILING_GB`] fallback ceiling. Production construction goes
    /// through [`Self::with_ceiling`] + [`startup_projection_ceiling_gb`]
    /// (decision #43) in `OrchestratedSystem::with_runner`.
    pub fn new(table: VramTable) -> Self {
        Self::with_ceiling(table, PROJECTION_CEILING_GB)
    }

    /// Construct with an explicit projection ceiling (decision #43: the
    /// startup-detected value; fixed for the enforcer's lifetime).
    pub fn with_ceiling(table: VramTable, ceiling_gb: f32) -> Self {
        Self { table, ceiling_gb }
    }

    /// The ceiling this enforcer admits under (GB).
    pub fn ceiling_gb(&self) -> f32 {
        self.ceiling_gb
    }

    /// Read access to the table (the scheduler reports the VRAM peak the M5 gate
    /// asserts against; the measurement harness overwrites rows via `table_mut`).
    pub fn table(&self) -> &VramTable {
        &self.table
    }

    /// Mutable access so the scheduler can route an OOM to
    /// [`VramTable::mark_unmeasured`] / the M5 harness to `set_measured` (doc 12 §7).
    pub fn table_mut(&mut self) -> &mut VramTable {
        &mut self.table
    }

    /// Project the VRAM for one load request (doc 04 R1, doc 12 §4, ADR-030):
    /// `active(weights + mmproj + kv_est(ctx) + img_act(n_images)) + framework
    /// + co_resident_weights`.
    ///
    /// `co_resident` is the set of models **already resident** (the scheduler's
    /// loaded set); the active model is excluded from the co-resident sum even if
    /// it appears there (re-run of an already-loaded model). An unknown model row
    /// projects `+inf` (never admitted) rather than silently passing.
    pub fn project(&self, req: LoadRequest, co_resident: &[ModelId]) -> Projection {
        let Some(p) = self.table.params(req.model) else {
            return Projection {
                projected_gb: f32::INFINITY,
                admit: false,
            };
        };
        let kv = if p.kv_ref_ctx_tokens > 0 {
            p.kv_ref_gb * (req.ctx_tokens as f32 / p.kv_ref_ctx_tokens as f32)
        } else {
            0.0
        };
        let img = p.img_act_per_image_gb * req.n_images as f32;
        // Co-resident weights (ADR-030): every OTHER loaded model's weights.
        // faster-whisper is CTranslate2, not llama.cpp — its ~2 GB figure
        // (ADR-024) already folds its runtime overhead, so `framework` is charged
        // once for the active llama.cpp context. On-target M5 measurement replaces
        // these seed rows wholesale (doc 12 §4).
        let co: f32 = co_resident
            .iter()
            .filter(|m| **m != req.model)
            .filter_map(|m| self.table.params(*m))
            .map(|q| q.weights_gb)
            .sum();
        let projected = p.weights_gb + p.mmproj_gb + kv + img + self.table.framework_gb + co;
        Projection {
            projected_gb: projected,
            admit: projected <= self.ceiling_gb,
        }
    }

    /// The next R3 rung to try for a failed `req` (doc 04 R3, ADR-030 order):
    /// first applicable of 7B->3B, ctx shrink (8K->4K->2K), unload co-resident
    /// STT, drop image, else `Refuse`. `Queue` is reserved (the mutex serializes
    /// contention). The ladder is VLM-admission-centric; an STT request that
    /// can't fit yields `Refuse` here — its L2 swap-victim path is M6 (doc 12 §3).
    pub fn next_rung(&self, req: LoadRequest, co_resident: &[ModelId]) -> DegradeRung {
        if req.model == ModelId::Vlm7b {
            DegradeRung::DropTo3b
        } else if req.ctx_tokens > CTX_SHRINK_HIGH {
            DegradeRung::ShrinkCtx {
                next_ctx_tokens: CTX_SHRINK_HIGH,
            }
        } else if req.ctx_tokens > CTX_SHRINK_LOW {
            DegradeRung::ShrinkCtx {
                next_ctx_tokens: CTX_SHRINK_LOW,
            }
        } else if !is_stt(req.model) && co_resident.iter().copied().any(is_stt) {
            DegradeRung::UnloadCoResidentStt
        } else if req.n_images > 0 && !is_stt(req.model) {
            DegradeRung::DropImage
        } else {
            DegradeRung::Refuse
        }
    }

    /// Walk the R3 ladder to an admission decision (doc 04 §6, doc 12 §4). Each
    /// applied rung strictly reduces the request (model tier down, ctx down,
    /// co-resident count down), so the loop terminates. `DropImage`/`Queue`/
    /// `Refuse` are terminal for the scheduler — the caller degrades to OCR-only
    /// (doc 06 §6).
    pub fn admit(&self, req: LoadRequest, co_resident: &[ModelId]) -> Admission {
        let mut req = req;
        let mut co: Vec<ModelId> = co_resident.to_vec();
        let mut unload_stt = false;
        let mut degraded = false;
        loop {
            let p = self.project(req, &co);
            if p.admit {
                return Admission::Admit {
                    plan: req,
                    unload_stt,
                    projected_gb: p.projected_gb,
                    degraded,
                };
            }
            match self.next_rung(req, &co) {
                DegradeRung::DropTo3b => {
                    req.model = ModelId::Vlm3b;
                    degraded = true;
                }
                DegradeRung::ShrinkCtx { next_ctx_tokens } => {
                    req.ctx_tokens = next_ctx_tokens;
                    degraded = true;
                }
                DegradeRung::UnloadCoResidentStt => {
                    co.retain(|m| !is_stt(*m));
                    unload_stt = true;
                    degraded = true;
                }
                DegradeRung::DropImage | DegradeRung::Queue | DegradeRung::Refuse => {
                    return Admission::Refused {
                        projection_gb: p.projected_gb,
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enforcer() -> BudgetEnforcer {
        BudgetEnforcer::new(VramTable::seeded())
    }

    // --- decision #43: ceiling auto-scale ---------------------------------

    #[test]
    fn ceiling_derivation_subtracts_headroom_and_clamps() {
        // The RTX 5060's 8192 MiB reproduces the historical 7.0 exactly.
        assert!((ceiling_from_total_mib(8_192) - 7.0).abs() < 1e-4);
        // A 16 GB card gets 15.0; a 6 GB card gets 5.0.
        assert!((ceiling_from_total_mib(16_384) - 15.0).abs() < 1e-4);
        assert!((ceiling_from_total_mib(6_144) - 5.0).abs() < 1e-4);
        // Absurd parses clamp instead of poisoning the projection.
        assert_eq!(ceiling_from_total_mib(1_024), DETECTED_CEILING_MIN_GB);
        assert_eq!(ceiling_from_total_mib(10_000_000), DETECTED_CEILING_MAX_GB);
    }

    #[test]
    fn nvidia_smi_total_output_parses_first_gpu_line_only() {
        assert_eq!(parse_total_mib("8192\n"), Some(8192));
        assert_eq!(parse_total_mib("8192\r\n"), Some(8192), "Windows CRLF");
        assert_eq!(parse_total_mib(" 16384 \n8192\n"), Some(16384), "GPU 0 wins");
        assert_eq!(parse_total_mib(""), None);
        assert_eq!(parse_total_mib("N/A\n"), None);
    }

    #[test]
    fn new_defaults_to_the_fallback_ceiling() {
        assert_eq!(enforcer().ceiling_gb(), PROJECTION_CEILING_GB);
    }

    #[test]
    fn a_larger_detected_ceiling_admits_what_7gb_refused() {
        // 3B no-image + co-resident STT projects 7.27 GB: refused at 7.0 (see
        // `co_resident_stt_is_counted_conditionally`), admitted on a 12 GB
        // card's derived 11.0 ceiling (decision #43).
        let e = BudgetEnforcer::with_ceiling(VramTable::seeded(), ceiling_from_total_mib(12_288));
        let req = LoadRequest {
            model: ModelId::Vlm3b,
            ctx_tokens: 8192,
            n_images: 0,
        };
        assert!(e.project(req, &[ModelId::FasterWhisperSmall]).admit);
    }

    #[test]
    fn a_smaller_detected_ceiling_refuses_what_7gb_admitted() {
        // Lone 3B + image projects 6.47 GB: fits at 7.0 (see
        // `lone_3b_with_one_image_fits`), refused on a 6 GB card's 5.0 ceiling.
        let e = BudgetEnforcer::with_ceiling(VramTable::seeded(), ceiling_from_total_mib(6_144));
        let req = LoadRequest {
            model: ModelId::Vlm3b,
            ctx_tokens: 8192,
            n_images: 1,
        };
        assert!(!e.project(req, &[]).admit);
    }

    #[test]
    fn lone_3b_with_one_image_fits() {
        let e = enforcer();
        let req = LoadRequest {
            model: ModelId::Vlm3b,
            ctx_tokens: 8192,
            n_images: 1,
        };
        let p = e.project(req, &[]);
        // 1.93 + 1.34 + 1.0 + 1.2 + 1.0(fw) = 6.47 <= 7.0
        assert!(p.admit, "3B+image projected {} GB", p.projected_gb);
        assert!(p.projected_gb <= PROJECTION_CEILING_GB);
    }

    #[test]
    fn lone_7b_with_one_image_breaches_the_ceiling() {
        let e = enforcer();
        let req = LoadRequest {
            model: ModelId::Vlm7b,
            ctx_tokens: 8192,
            n_images: 1,
        };
        let p = e.project(req, &[]);
        // 4.68 + 1.35 + 2.0 + 1.2 + 1.0 = 10.23 > 7.0 (the doc 04 §2 "breaches").
        assert!(!p.admit, "7B+image must breach, projected {}", p.projected_gb);
    }

    #[test]
    fn co_resident_stt_is_counted_conditionally() {
        let e = enforcer();
        let req = LoadRequest {
            model: ModelId::Vlm3b,
            ctx_tokens: 8192,
            n_images: 0, // no image: 1.93+1.34+1.0+0+1.0 = 5.27
        };
        // + co-resident whisper weights 2.0 = 7.27 > 7.0 -> conditional co-res fails.
        let p = e.project(req, &[ModelId::FasterWhisperSmall]);
        assert!(!p.admit, "3B+STT co-resident projected {}", p.projected_gb);
        // Without STT it fits (co-residency is conditional, ADR-030).
        assert!(e.project(req, &[]).admit);
    }

    #[test]
    fn admit_drops_7b_to_3b_then_fits() {
        let e = enforcer();
        let req = LoadRequest {
            model: ModelId::Vlm7b,
            ctx_tokens: 8192,
            n_images: 1,
        };
        match e.admit(req, &[]) {
            Admission::Admit { plan, degraded, projected_gb, unload_stt } => {
                assert_eq!(plan.model, ModelId::Vlm3b, "7B degrades to 3B (R3 step 1)");
                assert!(degraded);
                assert!(!unload_stt);
                assert!(projected_gb <= PROJECTION_CEILING_GB);
            }
            other => panic!("expected Admit after 7B->3B, got {other:?}"),
        }
    }

    #[test]
    fn admit_unloads_co_resident_stt_before_admitting_image_vlm() {
        let e = enforcer();
        // 3B + image + co-resident STT: 1.93+1.34+1.0+1.2+1.0+2.0 = 8.47 > 7.0.
        let req = LoadRequest {
            model: ModelId::Vlm3b,
            ctx_tokens: 8192,
            n_images: 1,
        };
        match e.admit(req, &[ModelId::FasterWhisperSmall]) {
            Admission::Admit { unload_stt, plan, .. } => {
                assert!(unload_stt, "STT is the swap victim (ADR-030)");
                assert_eq!(plan.model, ModelId::Vlm3b);
            }
            other => panic!("expected Admit after unloading STT, got {other:?}"),
        }
    }

    #[test]
    fn next_rung_walks_the_ladder_in_order() {
        let e = enforcer();
        // 7B first rung.
        assert_eq!(
            e.next_rung(
                LoadRequest { model: ModelId::Vlm7b, ctx_tokens: 8192, n_images: 1 },
                &[]
            ),
            DegradeRung::DropTo3b
        );
        // 3B @ 8K -> shrink to 4K.
        assert_eq!(
            e.next_rung(
                LoadRequest { model: ModelId::Vlm3b, ctx_tokens: 8192, n_images: 1 },
                &[]
            ),
            DegradeRung::ShrinkCtx { next_ctx_tokens: 4096 }
        );
        // 3B @ 4K -> shrink to 2K.
        assert_eq!(
            e.next_rung(
                LoadRequest { model: ModelId::Vlm3b, ctx_tokens: 4096, n_images: 1 },
                &[]
            ),
            DegradeRung::ShrinkCtx { next_ctx_tokens: 2048 }
        );
        // 3B @ 2K with co-resident STT -> unload STT.
        assert_eq!(
            e.next_rung(
                LoadRequest { model: ModelId::Vlm3b, ctx_tokens: 2048, n_images: 1 },
                &[ModelId::FasterWhisperSmall]
            ),
            DegradeRung::UnloadCoResidentStt
        );
        // 3B @ 2K, no co-resident, still has image -> drop image.
        assert_eq!(
            e.next_rung(
                LoadRequest { model: ModelId::Vlm3b, ctx_tokens: 2048, n_images: 1 },
                &[]
            ),
            DegradeRung::DropImage
        );
        // Nothing left to reduce -> refuse.
        assert_eq!(
            e.next_rung(
                LoadRequest { model: ModelId::Vlm3b, ctx_tokens: 2048, n_images: 0 },
                &[]
            ),
            DegradeRung::Refuse
        );
    }

    /// The M5 gate property: `admit` NEVER returns an Admit whose plan projects
    /// over the 7.0 GB ceiling — for every model/ctx/image/co-resident combo.
    #[test]
    fn admitted_plans_never_exceed_the_ceiling() {
        let e = enforcer();
        let models = [
            ModelId::Vlm3b,
            ModelId::Vlm7b,
            ModelId::FasterWhisperSmall,
            ModelId::WhisperDistilLargeV3Int8,
        ];
        let co_sets: [&[ModelId]; 3] = [
            &[],
            &[ModelId::FasterWhisperSmall],
            &[ModelId::Vlm3b],
        ];
        for model in models {
            for ctx in [0u32, 512, 2048, 4096, 8192, 16384] {
                for n_images in [0u32, 1] {
                    for co in co_sets {
                        let req = LoadRequest { model, ctx_tokens: ctx, n_images };
                        if let Admission::Admit { plan, unload_stt, .. } = e.admit(req, co) {
                            // Re-project the admitted plan against the co-resident
                            // set the scheduler will actually have (STT gone iff
                            // unload_stt) — this must be within the ceiling.
                            let effective_co: Vec<ModelId> = if unload_stt {
                                co.iter().copied().filter(|m| !is_stt(*m)).collect()
                            } else {
                                co.to_vec()
                            };
                            let p = e.project(plan, &effective_co);
                            assert!(
                                p.admit && p.projected_gb <= PROJECTION_CEILING_GB,
                                "INVARIANT BREACH: admitted plan {plan:?} co={effective_co:?} \
                                 projects {} GB > {PROJECTION_CEILING_GB}",
                                p.projected_gb
                            );
                        }
                    }
                }
            }
        }
    }
}
