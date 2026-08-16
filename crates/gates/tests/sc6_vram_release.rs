//! SC6 gate — capture OFF releases the GPU within 3 s (doc 04 §, doc 05 §5,
//! doc 16 M1). The second permanent regression gate (doc 16, staged
//! recommendation 3).
//!
//! The promise (the capture-toggle invariant): killing the sidecars releases
//! their VRAM. Process death is the *only* guaranteed VRAM-release primitive
//! (doc 02 §2, doc 12 §5), which is what makes SC6 enforceable rather than
//! aspirational. After the kill:
//!   - GPU VRAM attributed to Aperture's model processes returns to ~0 within
//!     **3 s** (doc 01 SC6, doc 05 §5 "≤ 3 s SLA");
//!   - the whole sidecar tree (hosts AND their llama/whisper grandchildren —
//!     the Job Object guarantee, 2026-08-15) is dead.
//!
//! IMPLEMENTED 2026-08-15 (was todo!() since M5 — the live 2026-08-14 figures
//! [4.7 GB → 439 MB in < 3 s] are now a repeatable gate, not an anecdote).
//! The idle-CPU half of the M1 gate remains an app-level measurement (WPR /
//! PresentMon pass): this harness drives orchestration directly, so post-kill
//! CPU is trivially zero here and asserting it would be vacuous.
//!
//! **On-target only** (`#[ignore]`): requires the RTX target, `nvidia-smi` on
//! PATH, the dev-checkout sidecar assets (src-tauri\binaries + models\), and
//! NO other Aperture instance running (VRAM is attributed by process name).
//!
//! Shelling out to `nvidia-smi`/`tasklist` is allowed: the two-emitter rule
//! (doc 13 §2) governs network sockets and the Claude CLI, not local
//! measurement tools, and this never runs on the proactive path.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use aperture_orchestration::model_lifecycle::{ModelLifecycle, OsSpawner, SidecarConfig};
use aperture_orchestration::vram_table::ModelId;

/// Hard SLA: VRAM must return to ~0 within this window after the kill.
const RELEASE_SLA: Duration = Duration::from_secs(3);
/// "~0" tolerance, MiB — driver/context residue below this counts as released.
const VRAM_FLOOR_MIB: u64 = 64;
/// The GPU-holding grandchild process names VRAM is attributed to.
const GPU_PROCESS_NAMES: [&str; 2] = ["llama-server", "whisper-server"];
/// The host process images whose death the gate asserts.
const HOST_IMAGES: [&str; 2] = ["aperture-vlm-host.exe", "stt-host-x86_64-pc-windows-msvc.exe"];

/// Repo root (this file lives at crates/gates/tests/).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/gates has a repo root")
        .to_path_buf()
}

/// The dev-checkout sidecar layout `main::sidecar_config()` resolves — kept in
/// lockstep with src-tauri/src/main.rs (an on-target gate may assume the dev
/// checkout it runs from).
fn dev_sidecar_config(root: &PathBuf) -> SidecarConfig {
    let profile_dir = |bin: &str| {
        let release = root.join("target").join("release").join(bin);
        if release.exists() {
            release
        } else {
            root.join("target").join("debug").join(bin)
        }
    };
    SidecarConfig {
        vlm_host_bin: profile_dir("aperture-vlm-host.exe"),
        vlm_model_gguf: root.join("models").join("qwen2.5-vl-3b-q4_k_m.gguf"),
        vlm_mmproj_gguf: root.join("models").join("qwen2.5-vl-3b-mmproj-f16.gguf"),
        llama_bin: root.join("src-tauri").join("binaries").join("llama").join("llama-server.exe"),
        stt_host_bin: root
            .join("src-tauri")
            .join("binaries")
            .join("stt-host-x86_64-pc-windows-msvc.exe"),
        whisper_bin: root
            .join("src-tauri")
            .join("binaries")
            .join("whisper")
            .join("whisper-server.exe"),
        stt_model: root.join("models").join("ggml-base.en.bin"),
        stt_on_gpu: false,
        vlm_ctx: 4096,
        cold_load_timeout: Duration::from_secs(60), // cold llama load is slow; generous here
    }
}

/// Sum of `nvidia-smi` compute-app VRAM (MiB) for Aperture's model processes.
fn aperture_vram_mib() -> u64 {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=process_name,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .expect("nvidia-smi on PATH (on-target gate)");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let (name, mem) = line.rsplit_once(',')?;
            let name = name.trim().to_ascii_lowercase();
            GPU_PROCESS_NAMES
                .iter()
                .any(|p| name.contains(p))
                .then(|| mem.trim().parse::<u64>().ok())
                .flatten()
        })
        .sum()
}

/// Are any of the given process images alive? (`tasklist` image-name filter.)
fn any_process_alive(images: &[&str]) -> bool {
    images.iter().any(|img| {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("IMAGENAME eq {img}"), "/NH", "/FO", "CSV"])
            .output()
            .expect("tasklist available");
        String::from_utf8_lossy(&out.stdout).contains(img)
    })
}

#[test]
#[ignore = "SC6: on-target only — needs the RTX + nvidia-smi + dev sidecar assets, and no other Aperture instance running"]
fn sc6_toggle_off_releases_vram_within_3s_and_kills_sidecars() {
    let root = repo_root();
    let config = dev_sidecar_config(&root);
    for path in [&config.vlm_host_bin, &config.llama_bin, &config.vlm_model_gguf, &config.stt_host_bin] {
        assert!(
            path.exists(),
            "SC6 setup: missing sidecar asset {path:?} — build the hosts and fetch the models first"
        );
    }
    // Name-attributed VRAM demands exclusivity: another Aperture (or bare
    // llama/whisper server) would pollute both measurements.
    assert!(
        !any_process_alive(&["llama-server.exe", "whisper-server.exe"]),
        "SC6 setup: a llama/whisper server is already running — close Aperture first"
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let mut lifecycle = ModelLifecycle::new(Box::new(OsSpawner::new(config)));

    // Load BOTH sidecars so there is real VRAM to release.
    rt.block_on(async {
        lifecycle
            .ensure_loaded(ModelId::Vlm3b, 0)
            .await
            .expect("vlm-host + llama-server load");
        lifecycle
            .ensure_loaded(ModelId::FasterWhisperSmall, 0)
            .await
            .expect("stt-host + whisper-server load");
    });

    // Precondition: the VLM actually holds VRAM, else the gate is vacuous.
    // (whisper is the CPU build; llama alone is ~4+ GB.)
    let before = aperture_vram_mib();
    assert!(
        before > VRAM_FLOOR_MIB,
        "SC6 setup invalid: model processes hold only {before} MiB (≤ {VRAM_FLOOR_MIB} floor)"
    );
    assert!(
        any_process_alive(&HOST_IMAGES),
        "hosts must be alive before the kill"
    );

    // The toggle-OFF primitive: kill_all_sidecars (doc 12 §6 step 4). The Job
    // Object guarantees the grandchild dies with the host (2026-08-15 fix).
    let toggled_at = std::time::Instant::now();
    rt.block_on(async {
        lifecycle.kill_all_sidecars().await.expect("kill all");
    });

    // Poll VRAM at ~250 ms until it drops to ~0 or the SLA window expires.
    let mut released = false;
    while toggled_at.elapsed() < RELEASE_SLA {
        if aperture_vram_mib() <= VRAM_FLOOR_MIB {
            released = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let elapsed = toggled_at.elapsed();
    let after = aperture_vram_mib();
    assert!(
        released,
        "SC6 VIOLATION: model processes still hold {after} MiB after {elapsed:?} (> {RELEASE_SLA:?} SLA)"
    );

    // Process death is the release mechanism — assert the WHOLE tree is gone
    // (a surviving grandchild is exactly the orphan bug the Job Object fixed).
    assert!(
        !any_process_alive(&HOST_IMAGES),
        "SC6 VIOLATION: a sidecar host survived the kill"
    );
    assert!(
        !any_process_alive(&["llama-server.exe", "whisper-server.exe"]),
        "SC6 VIOLATION: a GPU grandchild survived the kill (orphaned VRAM)"
    );
}
