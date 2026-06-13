//! `aperture-stt-host` — the STT sidecar **binary** (doc 07 §3, doc 02 §2, doc 04 §3).
//!
//! This is a **standalone process**, not a library called in-process. The
//! Orchestration Resource Manager (doc 12 §5) spawns it on first PTT demand and
//! **kills** it to release VRAM. Process death is the only *guaranteed* way to
//! return VRAM to the driver (doc 02 §2); in-process bindings make unload
//! best-effort and would break the 8 GB ceiling and SC6 (< 3 s release on
//! capture-OFF). This binary owns one responsibility: launch, supervise, and — on
//! shutdown — **kill** a whisper.cpp / faster-whisper `server` child so that
//! killing *us* transitively reaps the GPU memory.
//!
//! Layout (this host) ──spawns──► whisper server child (the model lives in VRAM):
//! ```text
//!   orchestration ──spawn/kill──► [aperture-stt-host] ──spawn/kill──► whisper server
//!         ▲ POST /transcribe, GET /health (loopback, --port)   ▲ loopback HTTP
//! ```
//!
//! ## Invariants honored here
//! - **(1) 8 GB VRAM ceiling / single-GPU mutex.** This host does not arbitrate the
//!   mutex (doc 12 §3) — but STT is priority 100 and **never cancellable** (doc 12
//!   §3, doc 07 §3): once admitted, a transcribe job runs to completion. Whisper
//!   small ~1 GB (doc 04 §2) is cheap enough to warm-keep (doc 04 §5).
//! - **(2) two-emitter transparency gate.** This binary opens sockets **only** on
//!   loopback (`127.0.0.1:--port`) to its own whisper child. It is NOT the
//!   reasoning gateway: never the public internet, never the Claude CLI (doc 13 §2).
//! - **(3) capture toggle.** On parent-kill / `SIGTERM` we kill the child and exit;
//!   the guaranteed-release path is the parent killing *this* PID, backstopped by
//!   the OS-level child kill-on-drop (doc 12 §6 step 4).
//!
//! Model selection (doc 04 §3): **default = Whisper small (~1 GB; ≈95 % of large-v3
//! quality — the right default for an 8 GB card, doc 07 §3)**; **opt-in =
//! faster-whisper distil-large-v3 int8 (~1.48 GB measured)**. CPU fallback (whisper
//! small on CPU) when the GPU is unavailable (doc 07 §3, §6) — slower but functional.
//!
//! [VERIFY] exact whisper.cpp / faster-whisper server binary, flags, and measured
//! latencies → **SC4** (doc 07 §3, doc 04 §9 measurement plan; cold-load < 2 s).

// TODO(M6): voice milestone — implement the bodies below against the measured
// whisper server contract; record real-time-factor + latency into the SC4/M6 gate.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};

/// CLI surface. The orchestration Manager (doc 12 §2 `ModelLifecycle`) supplies
/// these on spawn; nothing is read from the environment so the spawn stays
/// reproducible/auditable.
#[derive(Debug, Parser)]
#[command(name = "aperture-stt-host", about = "Aperture STT sidecar (Whisper via whisper.cpp / faster-whisper)")]
pub struct Args {
    /// Loopback port for this host's own HTTP surface (`/transcribe`, `/health`).
    /// The parent pins this; we always bind `127.0.0.1` only (invariant 2).
    #[arg(long)]
    pub port: u16,

    /// Which inference backend to drive (doc 07 §3). [VERIFY] binary/flags at M6.
    #[arg(long, value_enum, default_value_t = Backend::WhisperCpp)]
    pub backend: Backend,

    /// Path to the whisper server executable (whisper.cpp `whisper-server` or the
    /// faster-whisper launcher). [VERIFY] name/flags at M6.
    #[arg(long, default_value = "whisper-server")]
    pub whisper_bin: PathBuf,

    /// Model weights. Default = **Whisper small** (~1 GB, doc 04 §2). Opt-in passes
    /// the distil-large-v3 int8 path (~1.48 GB) instead.
    #[arg(long)]
    pub model: PathBuf,

    /// `gpu` (default) or `cpu` (the doc 07 §3/§6 CPU fallback — slower, functional).
    #[arg(long, value_enum, default_value_t = Device::Gpu)]
    pub device: Device,

    /// Port the spawned whisper child listens on (loopback). Distinct from `--port`.
    #[arg(long, default_value_t = 0)]
    pub child_port: u16,
}

/// Inference backend selector (doc 07 §3).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Backend {
    /// whisper.cpp server — default for Whisper small (GGUF).
    WhisperCpp,
    /// faster-whisper — for distil-large-v3 int8 (opt-in, doc 04 §3).
    FasterWhisper,
}

/// Compute device (doc 07 §3 — GPU under the mutex, or CPU fallback).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Device {
    Gpu,
    Cpu,
}

/// `POST /transcribe` request body. 16 kHz mono PCM WAV (doc 07 §2; matches
/// [`aperture_contracts::GpuJobKind::Stt`]). VAD trimming already happened upstream
/// in the voice subsystem (doc 07 §2); the host just transcribes what it is given.
#[derive(Debug, Deserialize)]
pub struct TranscribeRequest {
    /// 16 kHz mono PCM WAV bytes.
    pub wav: Vec<u8>,
}

/// `POST /transcribe` success body. Mirrors the STT arm of
/// `aperture_contracts::JobOutput::Stt { transcript, avg_token_confidence, duration_ms }`
/// (doc 07 §3 output: transcript + avg token confidence + duration). The transcript
/// is **always** written as a `voice_utterance` event (telemetry role, locked
/// decision B — doc 07 §3); the confidence drives the doc 07 §4.4 confirm-chip path.
#[derive(Debug, Serialize, Deserialize)]
pub struct TranscribeResponse {
    pub transcript: String,
    /// Mean per-token confidence `[0.0, 1.0]`; < 0.6 ⇒ caller shows a confirm chip
    /// (doc 07 §4.4 — never act on a guess).
    pub avg_token_confidence: f32,
    /// Wall-clock transcription time; feeds the SC4 latency report (doc 04 §9).
    pub duration_ms: u32,
}

/// `GET /health` readiness body. The parent's `ModelLifecycle` polls this; readiness
/// = the whisper child reports its model loaded (doc 12 §5 "readiness = health OK").
#[derive(Debug, Serialize)]
pub struct Health {
    /// `true` once the whisper child has the model resident and is accepting jobs.
    pub ready: bool,
    /// Which model is resident (for the SC4 / M6 attribution report, doc 04 §9).
    pub model: String,
    /// Reflects `--device`: was the model loaded on GPU or the CPU fallback path?
    pub on_gpu: bool,
}

/// Errors surfaced to the parent. A child crash maps to orchestration
/// `JobError::SidecarDown`; doc 12 §5 / doc 07 §6: restart with backoff, and this
/// utterance falls back to CPU whisper.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("failed to spawn whisper server child: {0}")]
    Spawn(String),
    #[error("whisper child exited / is unreachable")]
    ChildDown,
    #[error("input was not 16 kHz mono PCM WAV")]
    BadAudio,
    #[error("transcription deadline exceeded")] // STT 15 s (doc 12 §3)
    Deadline,
    #[error("http error: {0}")]
    Http(String),
}

/// Supervises the whisper `server` child. Holds the child handle so that
/// dropping/killing **this** host transitively kills the child — the
/// guaranteed VRAM-release primitive (doc 02 §2, doc 12 §5).
pub struct WhisperChild {
    // child: tokio::process::Child,   // spawned with kill_on_drop(true)
    // base_url: String,               // http://127.0.0.1:<child_port>
}

impl WhisperChild {
    /// Spawn the whisper server with the backend/device flags, then poll its health
    /// until the model is resident or the cold-load SLA elapses (Whisper small
    /// < 2 s, doc 04 §5).
    ///
    /// [VERIFY] exact flags at M6, e.g. (illustrative, not final):
    /// whisper.cpp: `whisper-server -m <model> --host 127.0.0.1 --port <child_port>`;
    /// faster-whisper: launcher with `--compute_type int8 --device cuda`.
    pub async fn spawn(_args: &Args) -> Result<Self, HostError> {
        // TODO(M6):
        //   1. tokio::process::Command::new(args.whisper_bin)
        //        .kill_on_drop(true)            // invariant 3: kill => VRAM release
        //        .args([...measured backend/device flags...]);
        //   2. await child readiness via its /health within the cold-load SLA.
        //   3. record load latency + nvidia-smi VRAM delta for the M6/SC4 report.
        todo!("M6: spawn whisper server child with kill_on_drop; await readiness")
    }

    /// Forward one WAV job to the child and return the transcript + confidence +
    /// duration. STT is **never cancellable** (doc 12 §3): run to completion.
    pub async fn transcribe(&self, _req: &TranscribeRequest) -> Result<TranscribeResponse, HostError> {
        // TODO(M6): proxy WAV to child /inference; map JSON → TranscribeResponse;
        // compute avg_token_confidence + duration_ms; record RTF for SC4 (doc 04 §9).
        todo!("M6: proxy to whisper child; return transcript + confidence + duration")
    }

    /// Is the child alive and reporting the model resident?
    pub async fn is_ready(&self) -> bool {
        // TODO(M6): GET child /health and map to readiness.
        todo!("M6: probe whisper child readiness")
    }

    /// Explicitly kill the child (also happens on `Drop` via `kill_on_drop`). The
    /// *guaranteed* release path is the parent killing this host's PID.
    pub async fn kill(&mut self) -> Result<(), HostError> {
        // TODO(M6): child.kill().await; this is what returns VRAM to the driver.
        todo!("M6: kill whisper child => VRAM returned")
    }
}

/// Entry point. Parse args, spawn + supervise the child, serve loopback HTTP, and
/// kill the child on shutdown so VRAM is released (doc 02 §2, doc 12 §6 step 4).
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // Invariant 2: bind loopback ONLY. This host is not the gateway; it must never
    // be reachable off-box and never opens an outbound public socket (doc 13 §2).
    let _bind: (IpAddr, u16) = (IpAddr::V4(Ipv4Addr::LOCALHOST), args.port);

    // TODO(M6):
    //   1. let child = WhisperChild::spawn(&args).await? ;   // VRAM loads here
    //   2. build axum::Router:
    //        POST /transcribe -> decode TranscribeRequest -> child.transcribe() -> JSON
    //        GET  /health     -> Health { ready: child.is_ready().await, model, on_gpu }
    //   3. axum::serve(TcpListener::bind(_bind), router)
    //        .with_graceful_shutdown(ctrl_c / SIGTERM)
    //        — on shutdown: child.kill().await  (drop also kills, belt+braces).
    //   4. record cold-load + per-transcribe latency into the SC4/M6 report (doc 04 §9).
    let _ = args;
    todo!("M6: spawn child, serve loopback /transcribe + /health, kill child on shutdown")
}
