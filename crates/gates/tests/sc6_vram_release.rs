//! SC6 gate — capture OFF releases the GPU within 3 s (doc 04 §, doc 05 §5,
//! doc 16 M1). The second permanent regression gate (doc 16, staged
//! recommendation 3).
//!
//! The promise (the capture-toggle invariant): toggling capture OFF stops the
//! sampler, releases the WGC session + UIA hooks, and **kills both sidecars**
//! (`vlm-host`, `stt-host`). Process death is the *only* guaranteed VRAM-release
//! primitive (doc 02 §2, doc 12 §5), which is what makes SC6 enforceable rather
//! than aspirational. After OFF:
//!   - GPU VRAM attributed to Aperture's models returns to ~0 within **3 s**
//!     (doc 01 SC6, doc 04 §, doc 05 §5 "≤ 3 s SLA");
//!   - the `vlm-host` and `stt-host` processes are dead;
//!   - idle CPU drops below **2 %** (doc 16 M1 gate).
//!
//! This gate is **on-target only**: it samples real VRAM via `nvidia-smi` and so
//! requires the RTX 5060 target (doc 04 §: `nvidia-smi --query-gpu=memory.used
//! -lms 250` around load/unload). It is `#[ignore]`-gated; CI on a GPU-less
//! runner skips it, and the M1 gate report runs it on hardware and overwrites the
//! `[VERIFY]` figure in doc 04.
//!
//! `toggle_capture(false)` is owned by the Orchestration Manager (the single
//! writer of toggle state, doc 05 §5); this test drives that entry point and
//! measures the consequence — it does not reach into the sidecars itself.

// TODO(M5/M1:): wire the real measurement + control surface. Three externals:
//   - VRAM: shell out to `nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits`
//     (per-process attribution via `--query-compute-apps=pid,used_memory` so we
//     measure *Aperture's* models, not the desktop's baseline). [VERIFY] flags.
//   - sidecar liveness: check the vlm-host/stt-host PIDs the orchestrator spawned.
//   - idle CPU: ETW / Windows Performance Recorder sample over a short window
//     (doc 04 §). Until orchestration + sidecars exist (M5), these are `todo!()`.
//
// NOTE: shelling out to `nvidia-smi` here is a `std::process::Command` spawn. That
// is allowed: the two-emitter rule (doc 13 §2) governs *network sockets and the
// Claude CLI*, not local measurement tools, and this is a `#[ignore]` on-target
// gate, never part of the egress-free proactive path. The xtask `lint-emitters`
// scanner should treat `crates/gates` as out of its proactive-path scope.

use std::time::Duration;

/// Hard SLA: VRAM must return to ~0 within this window after OFF (doc 05 §5).
const RELEASE_SLA: Duration = Duration::from_secs(3);
/// "~0" tolerance, MiB — driver/context residue below this counts as released.
/// [VERIFY] against the measured floor in the M1 gate report (doc 16).
const VRAM_FLOOR_MIB: u64 = 64;
/// Idle CPU ceiling after OFF (doc 16 M1 gate).
const IDLE_CPU_CEILING_PCT: f32 = 2.0;

/// A handle to the running system under test: the orchestrator with both sidecars
/// loaded. The real builder spawns `vlm-host` + `stt-host` and warms them so VRAM
/// is non-trivial before the toggle (otherwise the delta SC6 measures is zero).
struct LoadedSystem;

impl LoadedSystem {
    /// Bring capture ON and load both sidecars; block until VRAM is non-trivial.
    fn load_with_sidecars() -> Self {
        // TODO(M5:): start the orchestrator, spawn vlm-host + stt-host, run one
        // warm-up job each so VRAM is meaningfully > VRAM_FLOOR_MIB.
        todo!("M5: spawn + warm vlm-host & stt-host via the orchestrator")
    }

    /// Sample VRAM attributed to Aperture's models, in MiB (via `nvidia-smi`).
    fn sample_vram_mib(&self) -> u64 {
        todo!("M5: parse `nvidia-smi --query-compute-apps=pid,used_memory` for our sidecar PIDs")
    }

    /// True iff both sidecar processes are alive.
    fn sidecars_alive(&self) -> bool {
        todo!("M5: check the vlm-host & stt-host PIDs the orchestrator owns")
    }

    /// Current idle CPU usage for the Aperture process tree, percent.
    fn idle_cpu_pct(&self) -> f32 {
        todo!("M1: sample idle CPU over a short window (ETW / perf counter)")
    }

    /// The single sanctioned control surface (doc 05 §5): the Orchestration
    /// Manager flips toggle state and signals the sidecars to die.
    fn toggle_capture(&self, _on: bool) {
        todo!("M1: drive aperture_orchestration's capture-toggle entry point")
    }
}

#[test]
#[ignore = "SC6: requires the RTX 5060 target + nvidia-smi — runs at the M1 on-hardware gate (doc 16)"]
fn sc6_toggle_off_releases_vram_within_3s_and_kills_sidecars() {
    let sys = LoadedSystem::load_with_sidecars();

    // Precondition: there is real VRAM to release, else the gate is vacuous.
    let before = sys.sample_vram_mib();
    assert!(
        before > VRAM_FLOOR_MIB,
        "SC6 setup invalid: sidecars hold only {before} MiB (≤ floor); warm them first"
    );
    assert!(sys.sidecars_alive(), "both sidecars must be alive before OFF");

    // The toggle. The orchestrator kills the sidecars; process death frees VRAM.
    let toggled_at = std::time::Instant::now();
    sys.toggle_capture(false);

    // Poll VRAM until it drops to ~0 or the SLA window expires (doc 04 §:
    // `nvidia-smi ... -lms 250` — sample at ~250 ms cadence).
    let mut released = false;
    while toggled_at.elapsed() < RELEASE_SLA {
        if sys.sample_vram_mib() <= VRAM_FLOOR_MIB {
            released = true;
            break;
        }
        // TODO(M1:): sleep ~250 ms between samples (matching the nvidia-smi cadence)
        // without blocking a single-threaded test runtime.
        std::thread::sleep(Duration::from_millis(250));
    }

    let elapsed = toggled_at.elapsed();
    let after = sys.sample_vram_mib();
    assert!(
        released,
        "SC6 VIOLATION: VRAM still {after} MiB after {elapsed:?} (> {RELEASE_SLA:?} SLA)"
    );

    // Process death is the release mechanism — assert it actually happened.
    assert!(
        !sys.sidecars_alive(),
        "SC6 VIOLATION: VRAM dropped but a sidecar is still alive (best-effort unload, not a kill)"
    );

    // Idle CPU must settle below 2 % once the sampler + sidecars are gone.
    let cpu = sys.idle_cpu_pct();
    assert!(
        cpu < IDLE_CPU_CEILING_PCT,
        "SC6 VIOLATION: idle CPU {cpu:.1}% ≥ {IDLE_CPU_CEILING_PCT}% after OFF"
    );
}
