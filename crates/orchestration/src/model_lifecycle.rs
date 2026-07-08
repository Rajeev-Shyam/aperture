//! Sidecar lifecycle: spawn / kill / health / fallback (doc 12 §5, doc 04 §5).
//!
//! **Process death is the only guaranteed VRAM release** (doc 02 §2): killing a
//! sidecar is the unload primitive — what makes SC6 (< 3 s release) and the R1
//! ceiling *enforceable* rather than aspirational (doc 12 §5).
//!
//! Invariant (3): on capture OFF, [`kill_all_sidecars`](ModelLifecycle::kill_all_sidecars)
//! drives VRAM -> ~0 in < 3 s (doc 12 §6). Invariant (1): only one heavyweight
//! model per slot is ever resident (doc 02 Tier 1).
//!
//! NOTE (two-emitter rule, invariant 2): the [`std::process::Command`] sidecar
//! spawn in [`OsSpawner`] is the **ONLY sanctioned `Command` use outside the
//! reasoning gateway** (doc 13 §2). It spawns *local* model hosts only — never a
//! network socket nor the Claude CLI. A [`Spawner`] seam keeps the lifecycle
//! logic testable without the real llama.cpp binary + GGUF weights.

use std::collections::HashMap;
use std::time::Duration;

use crate::vram_table::ModelId;

/// Idle-unload after 60 s without a job (ADR-032/Q32; amended from R1's 90 s —
/// shorter windows also reduce risky co-residency, ADR-030) (doc 04 §5).
pub const IDLE_UNLOAD: Duration = Duration::from_secs(60);
/// L2 swap debounce: minimum residency before a model may be swapped out
/// (anti-thrash, doc 12 §7 [ASSUMPTION]).
pub const L2_MIN_RESIDENCY: Duration = Duration::from_secs(20);
/// Max crash-restarts before falling back (doc 12 §5).
pub const MAX_RESTART_ATTEMPTS: u8 = 3;

/// Which sidecar binary hosts a model (doc 02 §2). Pinned ports/pipes, readiness
/// = health endpoint OK (doc 12 §5). [VERIFY exact server binaries/flags.]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidecarKind {
    /// `vlm-host` — llama.cpp server + Qwen2.5-VL mmproj (doc 02 §2, doc 06 §3).
    VlmHost,
    /// `stt-host` — whisper.cpp / faster-whisper server (doc 02 §2, doc 07).
    SttHost,
}

impl SidecarKind {
    /// Which slot a model loads into (doc 04 §3).
    pub fn of(model: ModelId) -> Self {
        match model {
            ModelId::Vlm3b | ModelId::Vlm7b => SidecarKind::VlmHost,
            ModelId::FasterWhisperSmall | ModelId::WhisperDistilLargeV3Int8 => SidecarKind::SttHost,
        }
    }
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

/// Outcome of an L2 [`ModelLifecycle::l2_swap_to_stt`] (doc 12 §3/§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapOutcome {
    /// The resident VLM was evicted to free the slot for an STT arrival.
    /// `thrash_risk` is true when it had been resident less than
    /// [`L2_MIN_RESIDENCY`]: voice still wins (STT is never cancelled, doc 12 §3),
    /// but a *recurring* thrash-risk swap is the signal to recommend L1 to the
    /// user (doc 12 §7 [ASSUMPTION]).
    Swapped { thrash_risk: bool },
    /// The VLM slot was already free — nothing to evict; the STT loads directly.
    SlotAlreadyFree,
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

/// A spawned, health-passing sidecar process. Implementors own the OS handle and
/// must reap the child (and its transitive llama.cpp child) on `kill` — process
/// death is the guaranteed VRAM-release primitive (doc 02 §2). The [`Spawner`]
/// seam lets tests substitute a processless fake.
#[async_trait::async_trait]
pub trait SidecarProcess: Send + Sync {
    /// The loopback base URL of the sidecar's HTTP surface (`/infer`, `/health`).
    fn endpoint(&self) -> &str;
    /// Poll the health endpoint (doc 12 §5).
    async fn is_ready(&self) -> bool;
    /// Kill the process — the unload primitive. On a stuck child, escalate to
    /// `TerminateProcess` (doc 12 §7) before returning.
    async fn kill(&mut self) -> Result<(), LifecycleError>;
}

/// Spawns sidecars. Real = OS process + health poll ([`OsSpawner`]); tests use a
/// processless fake so the lifecycle logic (load/reuse/swap/kill/idle) is
/// exercised without the llama.cpp binary or GGUF weights.
#[async_trait::async_trait]
pub trait Spawner: Send + Sync {
    /// Spawn the sidecar hosting `model` and return the handle once it passes
    /// health within the cold-load SLA (3B < 4 s / 7B < 6 s / whisper < 2 s,
    /// doc 04 §5).
    async fn spawn(&self, model: ModelId) -> Result<Box<dyn SidecarProcess>, LifecycleError>;
}

/// One loaded slot: the process handle + the bookkeeping the timers read.
struct LoadedSidecar {
    process: Box<dyn SidecarProcess>,
    model: ModelId,
    /// Load-time stamp (epoch ms, the caller's clock) for the `L2_MIN_RESIDENCY`
    /// swap debounce (doc 12 §7). Read by `l2_swap_to_stt` to flag a thrash-risk
    /// swap — a VLM evicted while still inside its min-residency window.
    /// swap debounce (doc 12 §7). Its only reader is `l2_swap_to_stt`, which lands
    /// at M6 — stamped now so the swap has the datum it needs the day it's wired.
    #[allow(dead_code)] // M6: read by l2_swap_to_stt (todo! below)
    loaded_at_ms: i64,
    /// For `IDLE_UNLOAD` (doc 04 §5).
    last_job_at_ms: i64,
    /// Warm-keep pin (ADR-030/Q36: >=2 PTT/5 min pins STT). M6 sets it; the idle
    /// sweep honors it.
    warm_kept: bool,
}

/// Owns both sidecars and their warm-keep / swap policy (doc 04 §5, doc 12 §5).
/// Never resident with two heavyweight models in the same slot (invariant 1).
pub struct ModelLifecycle {
    spawner: Box<dyn Spawner>,
    slots: HashMap<SidecarKind, LoadedSidecar>,
    restart_attempts: HashMap<SidecarKind, u8>,
    fallbacks: HashMap<SidecarKind, Fallback>,
}

impl ModelLifecycle {
    /// Construct with a [`Spawner`]. Production wires [`OsSpawner`]; tests wire a
    /// processless fake. No sidecars are loaded until first demanded (doc 12 §6).
    pub fn new(spawner: Box<dyn Spawner>) -> Self {
        Self {
            spawner,
            slots: HashMap::new(),
            restart_attempts: HashMap::new(),
            fallbacks: HashMap::new(),
        }
    }

    /// The models currently resident (the scheduler's co-resident set for the R1
    /// projection, ADR-030).
    pub fn loaded_models(&self) -> Vec<ModelId> {
        self.slots.values().map(|s| s.model).collect()
    }

    /// Ensure `model` is loaded and ready; return its loopback endpoint. Reuses a
    /// ready slot holding the same model, else swaps (kills a different model in
    /// that slot, spawns the requested one). Demand-load on first job (doc 04 §5).
    /// Stamps `last_job_at` so the idle sweep sees fresh activity.
    pub async fn ensure_loaded(
        &mut self,
        model: ModelId,
        now_ms: i64,
    ) -> Result<String, LifecycleError> {
        let kind = SidecarKind::of(model);
        // Reuse a ready slot holding the same model.
        if let Some(slot) = self.slots.get_mut(&kind) {
            if slot.model == model && slot.process.is_ready().await {
                slot.last_job_at_ms = now_ms;
                return Ok(slot.process.endpoint().to_string());
            }
            // Wrong model (or unhealthy) in this slot — swap it out first.
            let mut old = self.slots.remove(&kind).expect("slot present");
            let _ = old.process.kill().await;
        }
        let process = self.spawner.spawn(model).await?;
        let endpoint = process.endpoint().to_string();
        self.restart_attempts.remove(&kind);
        self.fallbacks.remove(&kind);
        self.slots.insert(
            kind,
            LoadedSidecar {
                process,
                model,
                loaded_at_ms: now_ms,
                last_job_at_ms: now_ms,
                warm_kept: false,
            },
        );
        Ok(endpoint)
    }

    /// Kill one sidecar — the unload primitive (process death = guaranteed VRAM
    /// release, doc 12 §5). Idempotent: killing an empty slot is a no-op.
    pub async fn kill_sidecar(&mut self, kind: SidecarKind) -> Result<(), LifecycleError> {
        if let Some(mut slot) = self.slots.remove(&kind) {
            slot.process.kill().await?;
        }
        Ok(())
    }

    /// Kill **both** sidecars immediately — step 4 of the toggle-OFF sequence
    /// (doc 12 §6). No graceful drain on OFF: the 3 s SLA wins (invariant 3).
    /// Best-effort: a failed kill on one slot never blocks the other.
    pub async fn kill_all_sidecars(&mut self) -> Result<(), LifecycleError> {
        let mut first_err = None;
        for kind in [SidecarKind::VlmHost, SidecarKind::SttHost] {
            if let Some(mut slot) = self.slots.remove(&kind) {
                if let Err(e) = slot.process.kill().await {
                    tracing::error!(?kind, %e, "sidecar kill failed on OFF");
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Ping a sidecar's health endpoint (doc 12 §5).
    pub async fn health(&self, kind: SidecarKind) -> SidecarHealth {
        if let Some(fb) = self.fallbacks.get(&kind) {
            return SidecarHealth::Degraded(*fb);
        }
        if let Some(attempt) = self.restart_attempts.get(&kind) {
            if *attempt > 0 {
                return SidecarHealth::Restarting { attempt: *attempt };
            }
        }
        match self.slots.get(&kind) {
            Some(slot) if slot.process.is_ready().await => SidecarHealth::Ready,
            Some(_) => SidecarHealth::Loading,
            None => SidecarHealth::Down,
        }
    }

    /// On a crash, restart with exponential backoff; after `MAX_RESTART_ATTEMPTS`
    /// mark `Degraded` and fall back (VLM->OCR-only, STT->CPU, doc 12 §5). Returns
    /// the resulting health so the caller (scheduler) can route the degrade.
    pub async fn handle_crash(&mut self, kind: SidecarKind, model: ModelId, now_ms: i64) -> SidecarHealth {
        // Drop the dead slot.
        if let Some(mut slot) = self.slots.remove(&kind) {
            let _ = slot.process.kill().await;
        }
        let attempt = self.restart_attempts.entry(kind).or_insert(0);
        *attempt += 1;
        if *attempt > MAX_RESTART_ATTEMPTS {
            let fb = match kind {
                SidecarKind::VlmHost => Fallback::OcrOnly,
                SidecarKind::SttHost => Fallback::CpuWhisper,
            };
            self.fallbacks.insert(kind, fb);
            tracing::error!(?kind, "sidecar degraded after {MAX_RESTART_ATTEMPTS} restarts");
            return SidecarHealth::Degraded(fb);
        }
        let this_attempt = *attempt;
        // Exponential backoff before the respawn (doc 12 §5). Callers race this
        // against the job deadline — a slow respawn simply times the job out.
        let backoff = Duration::from_millis(250 * (1u64 << (this_attempt - 1)));
        tokio::time::sleep(backoff).await;
        match self.ensure_loaded(model, now_ms).await {
            Ok(_) => {
                self.restart_attempts.remove(&kind);
                SidecarHealth::Ready
            }
            Err(_) => SidecarHealth::Restarting { attempt: this_attempt },
        }
    }

    /// Idle-unload sweep: kill any sidecar idle > `IDLE_UNLOAD` (doc 04 §5).
    /// Honors the warm-keep policy (ADR-030/Q36: a warm-kept STT is pinned).
    pub async fn idle_sweep(&mut self, now_ms: i64) {
        let idle_ms = IDLE_UNLOAD.as_millis() as i64;
        let to_kill: Vec<SidecarKind> = self
            .slots
            .iter()
            .filter(|(_, s)| !s.warm_kept && now_ms - s.last_job_at_ms >= idle_ms)
            .map(|(k, _)| *k)
            .collect();
        for kind in to_kill {
            tracing::info!(?kind, "idle-unload after 60 s (doc 04 §5)");
            let _ = self.kill_sidecar(kind).await;
        }
    }

    /// Pin/unpin a sidecar's warm-keep (ADR-030/Q36 — >=2 PTT/5 min pins STT).
    /// M6 drives this from the PTT counter; exposed now so `idle_sweep` honors it.
    pub fn set_warm_kept(&mut self, kind: SidecarKind, warm: bool) {
        if let Some(slot) = self.slots.get_mut(&kind) {
            slot.warm_kept = warm;
        }
    }

    /// L2 only: evict the resident VLM so an arriving STT job can load into the
    /// (exclusive, doc 04 §3/§4) GPU. Voice is **never cancelled or delayed** for
    /// anti-thrash reasons (doc 12 §3: "an STT arrival additionally triggers the
    /// unload(7B)→load(whisper) swap; the swap time is charged to the STT job's
    /// thinking UI"), so the eviction is unconditional. The `L2_MIN_RESIDENCY`
    /// window (doc 12 §7 [ASSUMPTION]) is therefore reported as a *thrash-risk
    /// flag* on the outcome — not a block — so the caller can surface the
    /// "prefer L1" notice if swaps keep landing inside it. Process death frees the
    /// VLM's VRAM (doc 12 §5), so the STT load that follows fits under 7.0 GB
    /// (ADR-030). The caller then loads STT via [`ensure_loaded`](Self::ensure_loaded).
    pub async fn l2_swap_to_stt(&mut self, now_ms: i64) -> Result<SwapOutcome, LifecycleError> {
        let thrash_risk = match self.slots.get(&SidecarKind::VlmHost) {
            Some(slot) => {
                let resident_ms = now_ms.saturating_sub(slot.loaded_at_ms);
                resident_ms < L2_MIN_RESIDENCY.as_millis() as i64
            }
            None => return Ok(SwapOutcome::SlotAlreadyFree),
        };
        self.kill_sidecar(SidecarKind::VlmHost).await?;
        Ok(SwapOutcome::Swapped { thrash_risk })
    }
}

// ---------------------------------------------------------------------------
// The production spawner: OS process + loopback health poll.
// ---------------------------------------------------------------------------

/// Where the sidecar binaries + model weights live (from settings; [VERIFY] on
/// the real target — the GGUF weights are a metered download, deferred).
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// `aperture-vlm-host` executable (spawns llama.cpp; doc 02 §2).
    pub vlm_host_bin: std::path::PathBuf,
    /// Qwen2.5-VL GGUF weights (3B default / 7B opt-in, doc 04 §3).
    pub vlm_model_gguf: std::path::PathBuf,
    /// Vision projector (mmproj, FP16, doc 04 §2).
    pub vlm_mmproj_gguf: std::path::PathBuf,
    /// `aperture-stt-host` executable (M6).
    pub stt_host_bin: std::path::PathBuf,
    /// Context cap handed to the VLM sidecar (doc 04 R2).
    pub vlm_ctx: u32,
    /// Cold-load readiness deadline (doc 04 §5).
    pub cold_load_timeout: Duration,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            vlm_host_bin: std::path::PathBuf::from("aperture-vlm-host"),
            vlm_model_gguf: std::path::PathBuf::from("models/qwen2.5-vl-3b-q4_k_m.gguf"),
            vlm_mmproj_gguf: std::path::PathBuf::from("models/qwen2.5-vl-3b-mmproj-f16.gguf"),
            stt_host_bin: std::path::PathBuf::from("aperture-stt-host"),
            vlm_ctx: 4096,
            cold_load_timeout: Duration::from_secs(15),
        }
    }
}

/// Real spawner: launches `aperture-vlm-host` (the sanctioned `Command`, doc 13
/// §2) on a pinned loopback port and polls its `/health` until ready.
pub struct OsSpawner {
    config: SidecarConfig,
    /// Loopback port allocator (each spawn pins a distinct 127.0.0.1 port).
    next_port: std::sync::atomic::AtomicU16,
}

impl OsSpawner {
    /// Base of the pinned loopback port range for sidecars ([VERIFY] no clash).
    pub const PORT_BASE: u16 = 51_733;

    pub fn new(config: SidecarConfig) -> Self {
        Self {
            config,
            next_port: std::sync::atomic::AtomicU16::new(Self::PORT_BASE),
        }
    }
}

#[async_trait::async_trait]
impl Spawner for OsSpawner {
    async fn spawn(&self, model: ModelId) -> Result<Box<dyn SidecarProcess>, LifecycleError> {
        #[cfg(windows)]
        {
            os_spawn::spawn(&self.config, model, &self.next_port).await
        }
        #[cfg(not(windows))]
        {
            let _ = model;
            Err(LifecycleError::Spawn(
                "sidecars are Windows-only".to_string(),
            ))
        }
    }
}

#[cfg(windows)]
mod os_spawn {
    use super::*;
    use std::sync::atomic::{AtomicU16, Ordering};

    /// A running sidecar child + its loopback endpoint. Killing it reaps the
    /// transitive llama.cpp child (kill_on_drop), returning VRAM to the driver.
    pub(super) struct OsSidecar {
        child: tokio::process::Child,
        base_url: String,
        client: reqwest::Client,
    }

    #[async_trait::async_trait]
    impl SidecarProcess for OsSidecar {
        fn endpoint(&self) -> &str {
            &self.base_url
        }

        async fn is_ready(&self) -> bool {
            let url = format!("{}/health", self.base_url);
            match self.client.get(&url).send().await {
                Ok(resp) => resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v.get("ready").and_then(|r| r.as_bool()))
                    .unwrap_or(false),
                Err(_) => false,
            }
        }

        async fn kill(&mut self) -> Result<(), LifecycleError> {
            // Graceful child.kill() first; kill_on_drop is the belt-and-braces.
            if self.child.kill().await.is_ok() {
                return Ok(());
            }
            // Escalate to TerminateProcess on the raw handle (doc 12 §7).
            if let Some(pid) = self.child.id() {
                terminate_process(pid)?;
            }
            Ok(())
        }
    }

    pub(super) async fn spawn(
        config: &SidecarConfig,
        model: ModelId,
        next_port: &AtomicU16,
    ) -> Result<Box<dyn SidecarProcess>, LifecycleError> {
        if SidecarKind::of(model) == SidecarKind::SttHost {
            return Err(LifecycleError::Spawn("stt-host is the M6 milestone".into()));
        }
        let port = next_port.fetch_add(1, Ordering::Relaxed);
        let child_port = port.wrapping_add(1000);
        // The ONLY sanctioned std::process::Command outside the gateway (doc 13
        // §2): a local model host, loopback only, no network reach.
        let child = tokio::process::Command::new(&config.vlm_host_bin)
            .arg("--port")
            .arg(port.to_string())
            .arg("--model")
            .arg(&config.vlm_model_gguf)
            .arg("--mmproj")
            .arg(&config.vlm_mmproj_gguf)
            .arg("--ctx")
            .arg(config.vlm_ctx.to_string())
            .arg("--child-port")
            .arg(child_port.to_string())
            .kill_on_drop(true) // invariant 3: kill => VRAM release
            .spawn()
            .map_err(|e| LifecycleError::Spawn(e.to_string()))?;

        let base_url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| LifecycleError::Spawn(e.to_string()))?;
        let mut sidecar = OsSidecar {
            child,
            base_url,
            client,
        };

        // Poll /health until ready within the cold-load SLA (doc 04 §5).
        let deadline = tokio::time::Instant::now() + config.cold_load_timeout;
        while tokio::time::Instant::now() < deadline {
            if sidecar.is_ready().await {
                return Ok(Box::new(sidecar));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let _ = sidecar.kill().await;
        Err(LifecycleError::HealthTimeout)
    }

    /// Win32 `OpenProcess` + `TerminateProcess` escalation (doc 12 §7). The
    /// guaranteed-release backstop when `child.kill()` doesn't reap the tree.
    fn terminate_process(pid: u32) -> Result<(), LifecycleError> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, false, pid)
                .map_err(|e| LifecycleError::KillEscalated(e.to_string()))?;
            let result = TerminateProcess(handle, 1);
            let _ = CloseHandle(handle);
            result.map_err(|e| LifecycleError::KillEscalated(e.to_string()))
        }
    /// L2 only: unload the resident 7B and load Whisper for an STT arrival
    /// (residency is exclusive in L2, doc 04 §3/§4). Debounced by
    /// `L2_MIN_RESIDENCY` to avoid load thrash (doc 12 §7). M6 wires the STT job.
    pub async fn l2_swap_to_stt(&mut self, _now_ms: i64) -> Result<(), LifecycleError> {
        todo!("M6: L2 swap 7B->whisper with 20 s min-residency debounce (doc 12 §7)")
    }
}

// ---------------------------------------------------------------------------
// The production spawner: OS process + loopback health poll.
// ---------------------------------------------------------------------------

/// Where the sidecar binaries + model weights live (from settings; [VERIFY] on
/// the real target — the GGUF weights are a metered download, deferred).
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// `aperture-vlm-host` executable (spawns llama.cpp; doc 02 §2).
    pub vlm_host_bin: std::path::PathBuf,
    /// Qwen2.5-VL GGUF weights (3B default / 7B opt-in, doc 04 §3).
    pub vlm_model_gguf: std::path::PathBuf,
    /// Vision projector (mmproj, FP16, doc 04 §2).
    pub vlm_mmproj_gguf: std::path::PathBuf,
    /// `aperture-stt-host` executable (M6).
    pub stt_host_bin: std::path::PathBuf,
    /// Context cap handed to the VLM sidecar (doc 04 R2).
    pub vlm_ctx: u32,
    /// Cold-load readiness deadline (doc 04 §5).
    pub cold_load_timeout: Duration,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            vlm_host_bin: std::path::PathBuf::from("aperture-vlm-host"),
            vlm_model_gguf: std::path::PathBuf::from("models/qwen2.5-vl-3b-q4_k_m.gguf"),
            vlm_mmproj_gguf: std::path::PathBuf::from("models/qwen2.5-vl-3b-mmproj-f16.gguf"),
            stt_host_bin: std::path::PathBuf::from("aperture-stt-host"),
            vlm_ctx: 4096,
            cold_load_timeout: Duration::from_secs(15),
        }
    }
}

/// Real spawner: launches `aperture-vlm-host` (the sanctioned `Command`, doc 13
/// §2) on a pinned loopback port and polls its `/health` until ready.
pub struct OsSpawner {
    config: SidecarConfig,
    /// Loopback port allocator (each spawn pins a distinct 127.0.0.1 port).
    next_port: std::sync::atomic::AtomicU16,
}

impl OsSpawner {
    /// Base of the pinned loopback port range for sidecars ([VERIFY] no clash).
    pub const PORT_BASE: u16 = 51_733;

    pub fn new(config: SidecarConfig) -> Self {
        Self {
            config,
            next_port: std::sync::atomic::AtomicU16::new(Self::PORT_BASE),
        }
    }
}

#[async_trait::async_trait]
impl Spawner for OsSpawner {
    async fn spawn(&self, model: ModelId) -> Result<Box<dyn SidecarProcess>, LifecycleError> {
        #[cfg(windows)]
        {
            os_spawn::spawn(&self.config, model, &self.next_port).await
        }
        #[cfg(not(windows))]
        {
            let _ = model;
            Err(LifecycleError::Spawn(
                "sidecars are Windows-only".to_string(),
            ))
        }
    }
}

#[cfg(windows)]
mod os_spawn {
    use super::*;
    use std::sync::atomic::{AtomicU16, Ordering};

    /// A running sidecar child + its loopback endpoint. Killing it reaps the
    /// transitive llama.cpp child (kill_on_drop), returning VRAM to the driver.
    pub(super) struct OsSidecar {
        child: tokio::process::Child,
        base_url: String,
        client: reqwest::Client,
    }

    #[async_trait::async_trait]
    impl SidecarProcess for OsSidecar {
        fn endpoint(&self) -> &str {
            &self.base_url
        }

        async fn is_ready(&self) -> bool {
            let url = format!("{}/health", self.base_url);
            match self.client.get(&url).send().await {
                Ok(resp) => resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v.get("ready").and_then(|r| r.as_bool()))
                    .unwrap_or(false),
                Err(_) => false,
            }
        }

        async fn kill(&mut self) -> Result<(), LifecycleError> {
            // Graceful child.kill() first; kill_on_drop is the belt-and-braces.
            if self.child.kill().await.is_ok() {
                return Ok(());
            }
            // Escalate to TerminateProcess on the raw handle (doc 12 §7).
            if let Some(pid) = self.child.id() {
                terminate_process(pid)?;
            }
            Ok(())
        }
    }

    pub(super) async fn spawn(
        config: &SidecarConfig,
        model: ModelId,
        next_port: &AtomicU16,
    ) -> Result<Box<dyn SidecarProcess>, LifecycleError> {
        if SidecarKind::of(model) == SidecarKind::SttHost {
            return Err(LifecycleError::Spawn("stt-host is the M6 milestone".into()));
        }
        let port = next_port.fetch_add(1, Ordering::Relaxed);
        let child_port = port.wrapping_add(1000);
        // The ONLY sanctioned std::process::Command outside the gateway (doc 13
        // §2): a local model host, loopback only, no network reach.
        let child = tokio::process::Command::new(&config.vlm_host_bin)
            .arg("--port")
            .arg(port.to_string())
            .arg("--model")
            .arg(&config.vlm_model_gguf)
            .arg("--mmproj")
            .arg(&config.vlm_mmproj_gguf)
            .arg("--ctx")
            .arg(config.vlm_ctx.to_string())
            .arg("--child-port")
            .arg(child_port.to_string())
            .kill_on_drop(true) // invariant 3: kill => VRAM release
            .spawn()
            .map_err(|e| LifecycleError::Spawn(e.to_string()))?;

        let base_url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| LifecycleError::Spawn(e.to_string()))?;
        let mut sidecar = OsSidecar {
            child,
            base_url,
            client,
        };

        // Poll /health until ready within the cold-load SLA (doc 04 §5).
        let deadline = tokio::time::Instant::now() + config.cold_load_timeout;
        while tokio::time::Instant::now() < deadline {
            if sidecar.is_ready().await {
                return Ok(Box::new(sidecar));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let _ = sidecar.kill().await;
        Err(LifecycleError::HealthTimeout)
    }

    /// Win32 `OpenProcess` + `TerminateProcess` escalation (doc 12 §7). The
    /// guaranteed-release backstop when `child.kill()` doesn't reap the tree.
    fn terminate_process(pid: u32) -> Result<(), LifecycleError> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, TerminateProcess, PROCESS_TERMINATE,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, false, pid)
                .map_err(|e| LifecycleError::KillEscalated(e.to_string()))?;
            let result = TerminateProcess(handle, 1);
            let _ = CloseHandle(handle);
            result.map_err(|e| LifecycleError::KillEscalated(e.to_string()))
        }
    }
}

impl Default for ModelLifecycle {
    /// The production default: an [`OsSpawner`] with default paths (main.rs
    /// overrides from settings). Sidecars stay down until demanded.
    fn default() -> Self {
        Self::new(Box::new(OsSpawner::new(SidecarConfig::default())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    /// A processless sidecar: no OS child, always ready, records its kill.
    struct FakeSidecar {
        endpoint: String,
        killed: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl SidecarProcess for FakeSidecar {
        fn endpoint(&self) -> &str {
            &self.endpoint
        }
        async fn is_ready(&self) -> bool {
            true
        }
        async fn kill(&mut self) -> Result<(), LifecycleError> {
            self.killed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Counts spawns; hands out FakeSidecars whose kills flip a shared flag.
    #[derive(Default)]
    struct FakeSpawner {
        spawns: AtomicU32,
        vlm_killed: Arc<AtomicBool>,
        stt_killed: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Spawner for FakeSpawner {
        async fn spawn(&self, model: ModelId) -> Result<Box<dyn SidecarProcess>, LifecycleError> {
            let n = self.spawns.fetch_add(1, Ordering::SeqCst);
            let killed = match SidecarKind::of(model) {
                SidecarKind::VlmHost => Arc::clone(&self.vlm_killed),
                SidecarKind::SttHost => Arc::clone(&self.stt_killed),
            };
            Ok(Box::new(FakeSidecar {
                endpoint: format!("http://127.0.0.1:9{n:03}"),
                killed,
            }))
        }
    }

    #[tokio::test]
    async fn ensure_loaded_spawns_once_and_reuses() {
        let spawner = Arc::new(FakeSpawner::default());
        let mut life = ModelLifecycle::new(Box::new(SpawnerHandle(Arc::clone(&spawner))));
        let e1 = life.ensure_loaded(ModelId::Vlm3b, 0).await.unwrap();
        let e2 = life.ensure_loaded(ModelId::Vlm3b, 1).await.unwrap();
        assert_eq!(e1, e2, "same model reuses the slot");
        assert_eq!(spawner.spawns.load(Ordering::SeqCst), 1);
        assert_eq!(life.loaded_models(), vec![ModelId::Vlm3b]);
    }

    #[tokio::test]
    async fn swapping_the_model_in_a_slot_kills_the_old_one() {
        let spawner = Arc::new(FakeSpawner::default());
        let mut life = ModelLifecycle::new(Box::new(SpawnerHandle(Arc::clone(&spawner))));
        life.ensure_loaded(ModelId::Vlm3b, 0).await.unwrap();
        life.ensure_loaded(ModelId::Vlm7b, 1).await.unwrap();
        assert!(spawner.vlm_killed.load(Ordering::SeqCst), "3B killed for 7B swap");
        assert_eq!(life.loaded_models(), vec![ModelId::Vlm7b]);
        assert_eq!(spawner.spawns.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn kill_all_clears_both_slots() {
        let spawner = Arc::new(FakeSpawner::default());
        let mut life = ModelLifecycle::new(Box::new(SpawnerHandle(Arc::clone(&spawner))));
        life.ensure_loaded(ModelId::Vlm3b, 0).await.unwrap();
        life.ensure_loaded(ModelId::FasterWhisperSmall, 0).await.unwrap();
        assert_eq!(life.loaded_models().len(), 2);
        life.kill_all_sidecars().await.unwrap();
        assert!(life.loaded_models().is_empty());
        assert!(spawner.vlm_killed.load(Ordering::SeqCst));
        assert!(spawner.stt_killed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn idle_sweep_unloads_after_60s_but_honors_warm_keep() {
        let spawner = Arc::new(FakeSpawner::default());
        let mut life = ModelLifecycle::new(Box::new(SpawnerHandle(Arc::clone(&spawner))));
        life.ensure_loaded(ModelId::Vlm3b, 0).await.unwrap();
        life.ensure_loaded(ModelId::FasterWhisperSmall, 0).await.unwrap();
        life.set_warm_kept(SidecarKind::SttHost, true);
        // 61 s later: the VLM idles out, the warm-kept STT stays.
        life.idle_sweep(61_000).await;
        assert_eq!(life.loaded_models(), vec![ModelId::FasterWhisperSmall]);
    }

    #[tokio::test]
    async fn crash_degrades_after_max_restarts() {
        // A spawner that always fails to spawn simulates a persistent crash.
        struct DeadSpawner;
        #[async_trait::async_trait]
        impl Spawner for DeadSpawner {
            async fn spawn(&self, _m: ModelId) -> Result<Box<dyn SidecarProcess>, LifecycleError> {
                Err(LifecycleError::Spawn("boom".into()))
            }
        }
        let mut life = ModelLifecycle::new(Box::new(DeadSpawner));
        let mut health = SidecarHealth::Down;
        for _ in 0..=MAX_RESTART_ATTEMPTS {
            health = life
                .handle_crash(SidecarKind::VlmHost, ModelId::Vlm3b, 0)
                .await;
        }
        assert_eq!(health, SidecarHealth::Degraded(Fallback::OcrOnly));
    }

    #[tokio::test]
    async fn l2_swap_to_stt_evicts_a_resident_vlm_and_frees_the_slot() {
        let spawner = Arc::new(FakeSpawner::default());
        let mut life = ModelLifecycle::new(Box::new(SpawnerHandle(Arc::clone(&spawner))));
        // A 7B resident well past the min-residency window.
        life.ensure_loaded(ModelId::Vlm7b, 0).await.unwrap();
        let now = L2_MIN_RESIDENCY.as_millis() as i64 + 1_000;
        let outcome = life.l2_swap_to_stt(now).await.unwrap();
        assert_eq!(
            outcome,
            SwapOutcome::Swapped { thrash_risk: false },
            "a long-resident VLM is evicted with no thrash flag"
        );
        assert!(spawner.vlm_killed.load(Ordering::SeqCst), "the VLM is evicted");
        assert!(life.loaded_models().is_empty(), "the slot is free for the STT load");
    }

    #[tokio::test]
    async fn l2_swap_flags_thrash_risk_but_still_evicts_within_min_residency() {
        let spawner = Arc::new(FakeSpawner::default());
        let mut life = ModelLifecycle::new(Box::new(SpawnerHandle(Arc::clone(&spawner))));
        life.ensure_loaded(ModelId::Vlm7b, 0).await.unwrap();
        // Only 1 s resident — inside the 20 s debounce window. Voice still wins
        // (doc 12 §3); the swap is flagged, not blocked.
        let outcome = life.l2_swap_to_stt(1_000).await.unwrap();
        assert_eq!(outcome, SwapOutcome::Swapped { thrash_risk: true });
        assert!(spawner.vlm_killed.load(Ordering::SeqCst), "voice is never delayed for anti-thrash");
    }

    #[tokio::test]
    async fn l2_swap_is_a_noop_when_the_slot_is_already_free() {
        let spawner = Arc::new(FakeSpawner::default());
        let mut life = ModelLifecycle::new(Box::new(SpawnerHandle(Arc::clone(&spawner))));
        let outcome = life.l2_swap_to_stt(50_000).await.unwrap();
        assert_eq!(outcome, SwapOutcome::SlotAlreadyFree);
        assert!(!spawner.vlm_killed.load(Ordering::SeqCst));
    }

    /// Adapts an `Arc<FakeSpawner>` to the `Spawner` trait (tests share the
    /// spawner to read its counters).
    struct SpawnerHandle(Arc<FakeSpawner>);
    #[async_trait::async_trait]
    impl Spawner for SpawnerHandle {
        async fn spawn(&self, model: ModelId) -> Result<Box<dyn SidecarProcess>, LifecycleError> {
            self.0.spawn(model).await
        }
    }
}
