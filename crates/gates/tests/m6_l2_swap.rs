//! M6 gate — L1 conditional co-residency AND the L2 STT swap (doc 04 §3, doc 12
//! §3, doc 16 M6, ADR-030/ADR-024).
//!
//! The M6 promise the resource manager must keep (doc 16): **both** loadouts serve
//! voice without ever crossing the 7.0 GB ceiling —
//! - **L1 (3B default):** faster-whisper **conditionally co-resides** with the 3B
//!   VLM (their combined weights fit under 7.0 GB, so an STT arrival admits without
//!   evicting anything — ADR-030).
//! - **L2 (7B opt-in, exclusive):** the 7B + STT can't co-reside, so an STT arrival
//!   is **refused until the swap frees the slot**. `ModelLifecycle::l2_swap_to_stt`
//!   (unit-tested in `aperture-orchestration`) evicts the 7B; this gate proves the
//!   admission *consequence*: once the co-resident set no longer holds the 7B, STT
//!   admits. Lifecycle-evicts + enforcer-admits = the full swap (doc 12 §3).
//!
//! Pure R1 admission logic — no scheduler, GPU, mic, or network. A regression that
//! lets STT co-reside with the exclusive 7B (over budget) or that stops STT
//! admitting after the swap fails M6 here, in CI, forever. The **SC4 latency**
//! (< 2 s STT, GPU path) is the on-hardware companion, `#[ignore]`-gated below.
//!
//! The US2 end-to-end pipeline acceptance (tap-discard, unconditional store, the
//! query/escalation/telemetry branches, the < 0.6 confirm chip) lives in
//! `aperture-voice`'s own tests — driven offline with a `FakeScheduler` + in-memory
//! DB — and runs in the same `cargo test` sweep.

use aperture_orchestration::budget_enforcer::{
    Admission, BudgetEnforcer, LoadRequest, PROJECTION_CEILING_GB,
};
use aperture_orchestration::vram_table::{ModelId, VramTable};

fn enforcer() -> BudgetEnforcer {
    BudgetEnforcer::new(VramTable::seeded())
}

/// The STT load request that reaches the enforcer (doc 04 R2: STT is always ctx 0,
/// 0 images — its weights are the whole cost).
fn stt() -> LoadRequest {
    LoadRequest { model: ModelId::FasterWhisperSmall, ctx_tokens: 0, n_images: 0 }
}

#[test]
fn l1_stt_conditionally_co_resides_with_the_3b() {
    let e = enforcer();
    // An STT arrival while the L1 3B is resident: their combined weights fit under
    // 7.0 GB, so it admits co-resident — nothing is evicted (ADR-030 conditional
    // co-residency). This is the everyday L1 voice path.
    match e.admit(stt(), &[ModelId::Vlm3b]) {
        Admission::Admit { unload_stt, projected_gb, degraded, .. } => {
            assert!(!unload_stt, "STT admits alongside the 3B — it is not its own swap victim");
            assert!(!degraded, "STT has no degrade rung; it fits or it doesn't");
            assert!(
                projected_gb <= PROJECTION_CEILING_GB + f32::EPSILON,
                "co-resident 3B + STT must stay under {PROJECTION_CEILING_GB} GB (got {projected_gb:.2})"
            );
        }
        Admission::Refused { projection_gb } => {
            panic!("L1 co-residency must admit STT alongside the 3B ({projection_gb:.2} GB)");
        }
    }
}

#[test]
fn l2_stt_is_refused_against_the_resident_7b_before_the_swap() {
    let e = enforcer();
    // The exclusive 7B + STT blow the ceiling (4.68 + 1.35 mmproj + 2.0 + 1.0 fw ≫
    // 7.0), and STT has no R3 rung of its own — so the honest pre-swap answer is
    // Refused, never a co-residency over budget (doc 12 §3).
    assert!(
        matches!(e.admit(stt(), &[ModelId::Vlm7b]), Admission::Refused { .. }),
        "STT must refuse against a resident exclusive 7B until the swap frees the slot"
    );
}

#[test]
fn l2_stt_admits_once_the_swap_has_evicted_the_7b() {
    let e = enforcer();
    // After `ModelLifecycle::l2_swap_to_stt` kills the 7B (unit-tested there), the
    // scheduler re-admits with an empty co-resident set — STT now fits (2.0 + 1.0
    // framework = 3.0 GB) and admits. This is the admission half of the M6 L2 swap.
    match e.admit(stt(), &[]) {
        Admission::Admit { projected_gb, .. } => {
            assert!(
                projected_gb <= PROJECTION_CEILING_GB + f32::EPSILON,
                "post-swap STT must fit under {PROJECTION_CEILING_GB} GB (got {projected_gb:.2})"
            );
        }
        Admission::Refused { projection_gb } => {
            panic!("post-swap STT must admit into the freed slot ({projection_gb:.2} GB)");
        }
    }
}

#[test]
#[ignore = "SC4: requires the RTX 5060 target + a real whisper sidecar — runs at the M6 hardware gate (doc 16, doc 04 §9)"]
fn sc4_stt_latency_under_2s_for_a_short_utterance() {
    // On-hardware only: capture ~5–10 s of speech, enqueue the priority-100 STT
    // GpuJob, and assert end-to-end transcription (cold-load + inference) lands
    // under 2 s on the GPU path (doc 07 §3, doc 04 §9). The seeded gates above prove
    // the admission invariant; this proves the measured SLA. Wired with the
    // stt-host + measured whisper on the target, exactly like `m5_load_times`.
    unimplemented!("SC4 on-target: drive stt-host, time the transcribe, assert < 2 s");
}
