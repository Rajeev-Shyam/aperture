//! M5 gate — the VRAM admission ceiling (doc 12 §4, doc 16 M5, ADR-030).
//!
//! The promise (invariant 1, the 8 GB ceiling): the [`BudgetEnforcer`] must
//! **never admit a GPU job whose projected VRAM exceeds 7.0 GB** — the 1 GB
//! margin under the 8 GB card that keeps the projection safe against measurement
//! error, driver/compositor overhead, and fragmentation (doc 04 R1, ADR-030). The
//! projection counts **co-resident weights** (ADR-030's amendment from R1's 7.2),
//! which is the whole reason a VLM and STT cannot both fully reside during an
//! image job — they fast-swap (doc 12 §3).
//!
//! This gate runs everywhere (CPU-only, no GPU, no network): it drives the pure
//! R1+R3 admission logic over a matrix spanning both sanctioned loadouts and
//! asserts the ceiling holds by construction. The on-target companion
//! `m5_load_times` re-measures the seed rows on the RTX 5060 and re-asserts the
//! same invariant with *measured* numbers (doc 12 §4).
//!
//! Doc 16: "gate results overwrite the corresponding `[VERIFY]` figures" — a
//! regression that lets any admission cross 7.0 GB fails M5 here, in CI, forever.

use aperture_orchestration::budget_enforcer::{
    Admission, BudgetEnforcer, LoadRequest, PROJECTION_CEILING_GB,
};
use aperture_orchestration::vram_table::{ModelId, VramTable};

fn enforcer() -> BudgetEnforcer {
    BudgetEnforcer::new(VramTable::seeded())
}

/// The load requests that can actually reach the enforcer (doc 04 R2: VLM ≤ 1
/// image, STT always 0). Spans both loadouts (3B/L1, 7B/L2) and the ctx ladder.
fn candidate_requests() -> Vec<LoadRequest> {
    let mut reqs = Vec::new();
    for model in [ModelId::Vlm3b, ModelId::Vlm7b] {
        for ctx in [8192, 4096, 2048] {
            for n_images in [0, 1] {
                reqs.push(LoadRequest { model, ctx_tokens: ctx, n_images });
            }
        }
    }
    for model in [ModelId::FasterWhisperSmall, ModelId::WhisperDistilLargeV3Int8] {
        reqs.push(LoadRequest { model, ctx_tokens: 0, n_images: 0 });
    }
    reqs
}

/// The co-resident sets the scheduler can present (doc 12 §3): nothing, a
/// resident STT (the L1 conditional co-residency), or a resident VLM (STT
/// arriving against an image job). Two heavyweight VLMs never co-reside — same
/// slot (invariant 1) — so that combination is deliberately absent.
fn co_resident_sets() -> Vec<Vec<ModelId>> {
    vec![
        vec![],
        vec![ModelId::FasterWhisperSmall],
        vec![ModelId::Vlm3b],
        vec![ModelId::Vlm7b],
    ]
}

#[test]
fn no_admission_ever_exceeds_the_7gb_ceiling() {
    let e = enforcer();
    for req in candidate_requests() {
        for co in co_resident_sets() {
            match e.admit(req, &co) {
                Admission::Admit { plan, unload_stt, projected_gb, .. } => {
                    // The load-bearing M5 invariant: an admitted job's projection
                    // is under the ceiling — always (doc 04 R1, ADR-030).
                    assert!(
                        projected_gb <= PROJECTION_CEILING_GB + f32::EPSILON,
                        "M5 CEILING VIOLATION: admitted {req:?} over co={co:?} projects \
                         {projected_gb:.2} GB > {PROJECTION_CEILING_GB} GB (plan {plan:?}, \
                         unload_stt={unload_stt})"
                    );
                    // The admitted plan can only be a strict reduction of the ask.
                    assert!(
                        plan.ctx_tokens <= req.ctx_tokens && plan.n_images <= req.n_images,
                        "M5: admitted plan {plan:?} is not a reduction of {req:?}"
                    );
                }
                // Refusal is always ceiling-safe: the caller degrades to OCR-only
                // (doc 06 §6). Nothing to assert on the number.
                Admission::Refused { .. } => {}
            }
        }
    }
}

#[test]
fn adr030_vlm_image_and_stt_cannot_co_reside_stt_is_the_swap_victim() {
    let e = enforcer();
    // A 3B VLM with one image while faster-whisper is resident: their combined
    // weights blow the 7.0 GB ceiling, so the enforcer must resolve it by
    // unloading STT (the swap victim, ADR-030/Q38) — never by co-residing over
    // budget.
    let req = LoadRequest { model: ModelId::Vlm3b, ctx_tokens: 8192, n_images: 1 };
    match e.admit(req, &[ModelId::FasterWhisperSmall]) {
        Admission::Admit { unload_stt, projected_gb, .. } => {
            assert!(unload_stt, "ADR-030: STT must be the swap victim, not a co-resident");
            assert!(projected_gb <= PROJECTION_CEILING_GB + f32::EPSILON);
        }
        Admission::Refused { projection_gb } => {
            panic!("ADR-030: should admit by unloading STT, not refuse ({projection_gb:.2} GB)");
        }
    }
}

#[test]
fn l2_7b_with_an_image_degrades_rather_than_ooms() {
    let e = enforcer();
    // 7B + image alone already exceeds 7.0 GB; the R3 ladder drops 7B->3B before
    // ever admitting over budget (doc 04 R3 step 1).
    let req = LoadRequest { model: ModelId::Vlm7b, ctx_tokens: 8192, n_images: 1 };
    match e.admit(req, &[]) {
        Admission::Admit { plan, projected_gb, degraded, .. } => {
            assert!(degraded, "a 7B image job must degrade to fit");
            assert_eq!(plan.model, ModelId::Vlm3b, "R3 step 1: 7B -> 3B");
            assert!(projected_gb <= PROJECTION_CEILING_GB + f32::EPSILON);
        }
        Admission::Refused { projection_gb } => {
            panic!("a 7B image job should degrade to 3B, not refuse ({projection_gb:.2} GB)");
        }
    }
}

#[test]
fn stt_arriving_against_a_resident_7b_is_refused_until_the_m6_swap_lands() {
    let e = enforcer();
    // An STT request while the exclusive 7B is resident can't fit (2.0 + 1.0
    // framework + 4.68 co-resident 7B > 7.0) and has no R3 rung of its own — its
    // L2 swap-victim path is M6 (doc 12 §3 / budget_enforcer next_rung). Until
    // then the honest answer is Refused, never a co-residency over budget.
    let req = LoadRequest { model: ModelId::FasterWhisperSmall, ctx_tokens: 0, n_images: 0 };
    assert!(
        matches!(e.admit(req, &[ModelId::Vlm7b]), Admission::Refused { .. }),
        "STT-vs-resident-7B must refuse until the M6 L2 swap (doc 12 §3)"
    );
}

#[test]
fn the_common_l1_path_admits_undegraded() {
    let e = enforcer();
    // Sanity floor: a lone 3B + one image at full context is the everyday Layer-B
    // wake and must admit *without* degrading — else the ceiling is mis-seeded and
    // every enrichment would run downscaled.
    let req = LoadRequest { model: ModelId::Vlm3b, ctx_tokens: 8192, n_images: 1 };
    match e.admit(req, &[]) {
        Admission::Admit { degraded, unload_stt, projected_gb, .. } => {
            assert!(!degraded && !unload_stt, "the lone-3B path must not degrade");
            assert!(projected_gb <= PROJECTION_CEILING_GB + f32::EPSILON);
        }
        Admission::Refused { .. } => panic!("the lone-3B image path must admit"),
    }
    // And the non-negotiable value: the ceiling is exactly 7.0 GB (ADR-030,
    // amended from R1's 7.2). If this constant drifts, the invariant drifts.
    assert_eq!(PROJECTION_CEILING_GB, 7.0, "ADR-030: the projection ceiling is 7.0 GB");
}
