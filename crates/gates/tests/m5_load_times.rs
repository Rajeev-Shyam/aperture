//! M5 on-target gate — cold-load SLAs (SC3) + measured co-resident VRAM
//! (doc 04 §5, doc 12 §4, doc 16 M5, ADR-030).
//!
//! Two on-hardware promises the seeded gates can't prove without the RTX 5060:
//!
//! 1. **SC3 cold-load times** (doc 04 §5): a demand-loaded sidecar passes health
//!    within its SLA — **3B < 4 s, 7B < 6 s, whisper < 2 s**. Slower than this and
//!    the first Layer-B wake stalls past the VLM job deadline (doc 06 §6).
//! 2. **Measured VRAM ≤ 7.0 GB** (ADR-030): the seed rows in `vram_table` are
//!    doc 04 §2 *estimates*. This gate measures each model's real resident VRAM
//!    (via `nvidia-smi` per-process attribution), writes them back with
//!    [`VramTable::set_measured`], and re-asserts that the co-resident projections
//!    still hold under 7.0 GB — the measurement that overwrites the `[VERIFY]`
//!    figures in doc 04 (doc 16: "gate results overwrite the [VERIFY] figures").
//!
//! `#[ignore]`-gated: it spawns real sidecars and samples `nvidia-smi`, so it runs
//! only on the RTX target at the M5 hardware gate — never on a GPU-less CI runner.
//! Shelling out to `nvidia-smi` is a local measurement `Command`, not a network
//! socket or the Claude CLI, so it does not touch the two-emitter rule (doc 13 §2);
//! `crates/gates` is out of the `lint-emitters` proactive-path scope (see sc6).

use std::time::Duration;

use aperture_orchestration::budget_enforcer::{Admission, BudgetEnforcer, LoadRequest, PROJECTION_CEILING_GB};
use aperture_orchestration::vram_table::{ModelId, VramParams, VramTable};

/// Cold-load SLAs (doc 04 §5). Keyed by model; the on-hardware harness times the
/// health-pass after a demand load and asserts it lands under these.
const SLA_VLM3B: Duration = Duration::from_secs(4);
const SLA_VLM7B: Duration = Duration::from_secs(6);
const SLA_WHISPER: Duration = Duration::from_secs(2);

/// The on-target measurement surface: spawn a sidecar, time its health-pass, and
/// sample the VRAM its model actually holds. Real impl lands at the M5 hardware
/// gate; until then these are `todo!()` exactly like the SC6 surface.
struct Target;

impl Target {
    /// Demand-load `model`, block until its health endpoint passes, return how
    /// long that took (the SC3 cold-load time).
    fn cold_load(&self, _model: ModelId) -> Duration {
        todo!("M5 on-target: spawn the sidecar via the orchestrator, time the health-pass")
    }

    /// Resident VRAM for `model` in GB, via `nvidia-smi --query-compute-apps`
    /// per-process attribution (the sidecar PID), so the desktop baseline is
    /// excluded.
    fn measured_params(&self, _model: ModelId) -> VramParams {
        todo!("M5 on-target: nvidia-smi per-process VRAM -> a measured VramParams row")
    }
}

fn sla_for(model: ModelId) -> Duration {
    match model {
        ModelId::Vlm3b => SLA_VLM3B,
        ModelId::Vlm7b => SLA_VLM7B,
        ModelId::FasterWhisperSmall | ModelId::WhisperDistilLargeV3Int8 => SLA_WHISPER,
    }
}

#[test]
#[ignore = "SC3: requires the RTX 5060 target + real sidecars — runs at the M5 hardware gate (doc 16)"]
fn cold_load_times_meet_the_sc3_slas() {
    let target = Target;
    for model in [
        ModelId::Vlm3b,
        ModelId::Vlm7b,
        ModelId::FasterWhisperSmall,
        ModelId::WhisperDistilLargeV3Int8,
    ] {
        let took = target.cold_load(model);
        let sla = sla_for(model);
        assert!(
            took <= sla,
            "SC3 VIOLATION: {model:?} cold-loaded in {took:?} > SLA {sla:?} (doc 04 §5)"
        );
    }
}

#[test]
#[ignore = "ADR-030: requires the RTX 5060 target + nvidia-smi — runs at the M5 hardware gate (doc 16)"]
fn measured_co_resident_vram_still_holds_under_7gb() {
    let target = Target;
    // Overwrite every seed row with its measured VRAM, then re-run the same
    // ceiling invariant the CPU gate proved on estimates — now on real numbers.
    let mut table = VramTable::seeded();
    for model in [
        ModelId::Vlm3b,
        ModelId::Vlm7b,
        ModelId::FasterWhisperSmall,
        ModelId::WhisperDistilLargeV3Int8,
    ] {
        table.set_measured(model, target.measured_params(model));
    }
    let e = BudgetEnforcer::new(table);

    // The ADR-030 keystone: a 3B image job with STT resident must still resolve
    // by unloading STT and land under 7.0 GB with *measured* weights.
    let req = LoadRequest { model: ModelId::Vlm3b, ctx_tokens: 8192, n_images: 1 };
    match e.admit(req, &[ModelId::FasterWhisperSmall]) {
        Admission::Admit { unload_stt, projected_gb, .. } => {
            assert!(unload_stt, "ADR-030: STT is the swap victim under measured numbers too");
            assert!(
                projected_gb <= PROJECTION_CEILING_GB + f32::EPSILON,
                "M5 CEILING VIOLATION on measured VRAM: {projected_gb:.2} GB > {PROJECTION_CEILING_GB} GB"
            );
        }
        Admission::Refused { projection_gb } => {
            panic!("measured 3B+image+STT should admit by unloading STT, not refuse ({projection_gb:.2} GB)");
        }
    }
}
