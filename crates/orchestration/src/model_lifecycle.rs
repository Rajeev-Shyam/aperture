//! Sidecar lifecycle: spawn / kill / health / fallback (doc 12 §5, doc 04 §5).
//!
//! **Process death is the only guaranteed VRAM release** (doc 02 §2): killing a
//! sidecar is the unload primitive — what makes SC6 (< 3 s release) and the R1
//! ceiling *enforceable* rather than aspirational (doc 12 §5).
//!
//! Invariant (3): on capture OFF, [`kill_all_sidecars`](ModelLifecycle::kill_all_sidecars)
//! drives VRAM -> ~0 in < 3 s (doc 12 §6). Invariant (1): only one heavyweight
//! model is ever resident (doc 02 Tier 1).
//!
//! NOTE (two-emitter rule, invariant 2): the [`std::process::Command`] sidecar
//! spawn below is the **ONLY sanctioned `Command` use outside the reasoning
//! gateway** (doc 13 §2). It spawns *local* model hosts only — it never opens a
//! network socket nor spawns the Claude CLI.

use std::time::Duration;

use crate::vram_table::ModelId;

/// Idle-unload after 90 s without a job (range 60–120, default 90 — doc 04 §5).
pub const IDLE_UNLOAD: Duration = Duration::from_secs(90);
/// L2 swap debounce: minimum residency before a model may be swapped out
/// (anti-thrash, doc 12 §7 [ASSUMPTION]).
pub const L2_MIN_RESIDENCY: Duration = Duration::from_secs(20);
/// Max crash-restarts before falling back (doc 12 §5).
pub const MAX_RESTART_ATTEMPTS: u8 = 3;

/// Which sidecar binary hosts a model (doc 02 §2). Pinned ports/pipes, readiness
/// = health endpoint OK (doc 12 §5). [VERIFY exact server binaries/flags.]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarKind {
    /// `vlm-host` — llama.cpp server + Qwen2.5-VL mmproj (doc 02 §2, doc 06 §3).
    VlmHost,
    /// `stt-host` — whisper.cpp / faster-whisper server (doc 02 §2, doc 07).
    SttHost,
}

/// The degraded fallback a sidecar drops to after `MAX_RESTART_ATTEMPTS`
/// (doc 12 §5): VLM -> OCR-only, STT -> CPU whisper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// VLM gave up: the pipeline proceeds on OCR text alone (doc 06 §6).
    OcrOnly,
    /// STT gave up: transcribe on CPU whisper instead of the GPU sidecar (doc 12 §5).
    CpuWhisper,
}

/// Sidecar health, from the readiness/health endpoint ping (doc 12 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarHealth {
    /// Spawned, health endpoint OK, model resident — usable now.
    Ready,
    /// Spawned, not yet passing health (cold-loading, doc 04 §5 SLAs).
    Loading,
    /// Crashed; will restart with exponential backoff (attempt N of 3).
    Restarting { attempt: u8 },
    /// Gave up after 3 attempts; running on `Fallback` (doc 12 §5).
    Degraded(Fallback),
    /// Not loaded (never spawned or idle-unloaded / killed).
    Down,
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("failed to spawn sidecar: {0}")]
    Spawn(String),
    #[error("sidecar never became healthy within the cold-load deadline")]
    HealthTimeout,
    #[error("kill failed; escalated to TerminateProcess: {0}")]
    KillEscalated(String),
    #[error("VRAM did not confirm release after kill (zombie?)")]
    VramNotReleased,
}

/// A spawned sidecar process handle. Wraps the OS `Child` plus its pinned
/// endpoint and current health.
pub struct Sidecar {
    // child: std::process::Child,   // the spawned vlm-host / stt-host
    // endpoint: Endpoint,           // pinned port / named pipe (doc 12 §5)
    // model: ModelId,
    // health: SidecarHealth,
    // loaded_at: std::time::Instant, // for L2_MIN_RESIDENCY debounce (doc 12 §7)
    // last_job_at: std::time::Instant, // for IDLE_UNLOAD (doc 04 §5)
}

/// Owns both sidecars and their warm-keep / swap policy (doc 04 §5, doc 12 §5).
/// Never resident with two heavyweight models at once (invariant 1).
pub struct ModelLifecycle {
    // vlm: Option<Sidecar>,
    // stt: Option<Sidecar>,
    // restart_attempts: HashMap<SidecarKind, u8>,
}

impl ModelLifecycle {
    /// No sidecars loaded; ON reverses lazily — sidecars stay down until first
    /// demanded (doc 12 §6).
    pub fn new() -> Self {
        Self {}
    }

    /// Spawn a sidecar to host `model` and wait for its health endpoint to go OK
    /// (readiness, doc 12 §5). Demand-load on first job (doc 04 §5).
    ///
    /// This is the **only sanctioned `std::process::Command` use outside the
    /// gateway** (invariant 2, doc 13 §2): it spawns a local model host with
    /// pinned ports/pipes; it must never reach the network.
    pub async fn spawn_sidecar(&mut self, _model: ModelId) -> Result<(), LifecycleError> {
        // TODO(M5:) std::process::Command::new(<vlm-host|stt-host bin>)
        //   .args([pinned port/pipe, model path, mmproj, ctx, flags]).spawn();
        //   then poll the health endpoint until Ready within the cold-load SLA
        //   (3B < 4 s / 7B < 6 s / whisper < 2 s, doc 04 §5). [VERIFY bins/flags]
        // M6 adds the stt-host path (doc 16 M6).
        todo!("M5: spawn vlm-host (M6: stt-host); the only sanctioned Command outside the gateway")
    }

    /// Kill one sidecar — the unload primitive (process death = guaranteed VRAM
    /// release, doc 12 §5). On a failed kill, escalate to `TerminateProcess`
    /// (doc 12 §7) and refuse new loads until VRAM telemetry confirms release.
    pub async fn kill_sidecar(&mut self, _kind: SidecarKind) -> Result<(), LifecycleError> {
        // TODO(M5:) child.kill(); if it lingers -> Win32 OpenProcess +
        //   TerminateProcess (windows crate); then confirm via the VRAM sampler
        //   (doc 04 §9) before allowing the next load (zombie escalation, doc 12 §7).
        todo!("M5: kill = unload; escalate to TerminateProcess on failure (doc 12 §7)")
    }

    /// Kill **both** sidecars immediately — step 4 of the toggle-OFF sequence
    /// (doc 12 §6). No graceful drain on OFF: the 3 s SLA wins (invariant 3).
    pub async fn kill_all_sidecars(&mut self) -> Result<(), LifecycleError> {
        // TODO(M1/M5:) kill vlm + stt unconditionally; this is the VRAM -> ~0
        //   half of the < 3 s SC6 release (doc 12 §6). Wired into ToggleOwner.
        todo!("M1: kill both sidecars on OFF — no drain, SLA wins (doc 12 §6)")
    }

    /// Ping a sidecar's health endpoint (doc 12 §5).
    pub async fn health(&self, _kind: SidecarKind) -> SidecarHealth {
        // TODO(M5:) hit the local health endpoint; map to SidecarHealth.
        todo!("M5: sidecar health ping (doc 12 §5)")
    }

    /// On a crash, restart with exponential backoff; after `MAX_RESTART_ATTEMPTS`
    /// mark `Degraded` and fall back (VLM->OCR-only, STT->CPU, doc 12 §5).
    pub async fn handle_crash(&mut self, _kind: SidecarKind) -> SidecarHealth {
        // TODO(M5:) attempt counter + exp backoff; on the 4th failure return
        //   Degraded(OcrOnly|CpuWhisper) and stop restarting (doc 12 §5).
        todo!("M5: exp backoff max 3 then fallback (doc 12 §5)")
    }

    /// Idle-unload sweep: kill any sidecar idle > `IDLE_UNLOAD` (doc 04 §5).
    /// Honors the warm-keep policy (>=3 PTT uses in 10 min pins Whisper, makes
    /// the VLM the swap victim — doc 04 §5).
    pub async fn idle_sweep(&mut self) {
        // TODO(M5:) for each loaded sidecar, if now - last_job_at > IDLE_UNLOAD
        //   and not warm-kept -> kill_sidecar (doc 04 §5).
        todo!("M5: idle-unload after 90 s; honor warm-keep (doc 04 §5)")
    }

    /// L2 only: unload the resident 7B and load Whisper for an STT arrival
    /// (residency is exclusive in L2, doc 04 §3/§4). Debounced by
    /// `L2_MIN_RESIDENCY` to avoid load thrash (doc 12 §7); the swap time is
    /// charged to the STT job's "thinking" UI (doc 07 §6).
    pub async fn l2_swap_to_stt(&mut self) -> Result<(), LifecycleError> {
        // TODO(M6:) if 7B has been resident < L2_MIN_RESIDENCY, the caller may
        //   warn (recommend L1) rather than thrash; else kill(VlmHost) then
        //   spawn_sidecar(WhisperSmall) (doc 12 §7, doc 07 §6).
        todo!("M6: L2 swap 7B->whisper with 20 s min-residency debounce (doc 12 §7)")
    }
}

impl Default for ModelLifecycle {
    fn default() -> Self {
        Self::new()
    }
}
