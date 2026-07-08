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
//!   §3, doc 07 §3): once admitted, a transcribe job runs to completion.
//!   faster-whisper small is ~2 GB on GPU (ADR-024/ADR-030, doc 04 §2) — the
//!   figure behind the 7.0 GB cap and conditional L1 co-residency; it is the
//!   designated swap victim when the projection doesn't fit.
//! - **(2) two-emitter transparency gate.** This binary opens sockets **only** on
//!   loopback (`127.0.0.1:--port`) to its own whisper child. It is NOT the
//!   reasoning gateway: never the public internet, never the Claude CLI (doc 13 §2).
//! - **(3) capture toggle.** On parent-kill / `SIGTERM` we kill the child and exit;
//!   the guaranteed-release path is the parent killing *this* PID, backstopped by
//!   the OS-level child kill-on-drop (doc 12 §6 step 4).
//!
//! Model selection (ADR-024, doc 04 §3): **default = faster-whisper (CTranslate2)
//! small, ~2 GB GPU** — ≈95 % of large-v3 quality, the right default for an 8 GB
//! card (doc 07 §3). CPU fallback = **whisper.cpp base (default) / tiny** when the
//! GPU is unavailable or STT is swapped out (doc 07 §3, §6) — slower but functional.
//!
//! ## Status (M6, best-effort)
//! The child supervision, loopback HTTP surface, kill-on-drop, and response parse
//! are implemented; [`parse_transcription`] / [`is_wav`] are pure + unit-tested.
//! The child wire contract (whisper.cpp `/inference` multipart shape, health probe,
//! GPU flags) is **UNVERIFIED** and [VERIFY]-tagged — it is confirmed on the RTX
//! box with real weights at the SC4 gate (doc 04 §9; cold-load < 2 s).

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

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
#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
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
    child: Mutex<tokio::process::Child>,
    /// `http://127.0.0.1:<child_port>` — the whisper server's loopback surface.
    base_url: String,
    /// Which model is resident (for the `/health` attribution).
    model_label: String,
    /// Reflects `--device` for the health report.
    on_gpu: bool,
    client: reqwest::Client,
}

impl WhisperChild {
    /// Spawn the whisper server with the backend/device flags, then poll its health
    /// until the model is resident or the cold-load SLA elapses (Whisper small
    /// < 2 s, doc 04 §5 — generous ceiling here for cold disk).
    ///
    /// [VERIFY] exact flags at M6 (illustrative, not final):
    /// whisper.cpp: `whisper-server -m <model> --host 127.0.0.1 --port <child_port>`;
    /// faster-whisper: launcher with `--compute_type int8 --device cuda`.
    pub async fn spawn(args: &Args) -> Result<Self, HostError> {
        let child_port = if args.child_port == 0 {
            args.port.wrapping_add(1)
        } else {
            args.child_port
        };

        let mut cmd = tokio::process::Command::new(&args.whisper_bin);
        cmd.arg("-m")
            .arg(&args.model)
            .arg("--host")
            .arg("127.0.0.1") // invariant 2: loopback only, never off-box
            .arg("--port")
            .arg(child_port.to_string());
        // Device selection [VERIFY on target]: whisper.cpp with a CUDA build offloads
        // to GPU by default; the CPU fallback disables GPU. faster-whisper takes
        // `--device cuda|cpu` instead — wired here when that backend is selected.
        match (args.backend, args.device) {
            (Backend::WhisperCpp, Device::Cpu) => {
                cmd.arg("-ng"); // whisper.cpp: no GPU
            }
            (Backend::FasterWhisper, dev) => {
                cmd.arg("--device")
                    .arg(if dev == Device::Gpu { "cuda" } else { "cpu" });
            }
            (Backend::WhisperCpp, Device::Gpu) => {}
        }
        cmd.kill_on_drop(true); // invariant 3: kill => VRAM release
        let child = cmd.spawn().map_err(|e| HostError::Spawn(e.to_string()))?;

        let base_url = format!("http://127.0.0.1:{child_port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| HostError::Http(e.to_string()))?;
        let host = Self {
            child: Mutex::new(child),
            base_url,
            model_label: args
                .model
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "whisper-small".into()),
            on_gpu: args.device == Device::Gpu,
            client,
        };

        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if host.is_ready().await {
                return Ok(host);
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        Err(HostError::ChildDown)
    }

    /// Forward one WAV job to the child and return the transcript + confidence +
    /// duration. STT is **never cancellable** (doc 12 §3): run to completion.
    ///
    /// [VERIFY] the whisper.cpp `/inference` multipart contract on-target; the
    /// response parse ([`parse_transcription`]) is pure + tested.
    pub async fn transcribe(&self, req: &TranscribeRequest) -> Result<TranscribeResponse, HostError> {
        if !is_wav(&req.wav) {
            return Err(HostError::BadAudio);
        }
        let started = Instant::now();
        let part = reqwest::multipart::Part::bytes(req.wav.clone())
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| HostError::Http(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("response_format", "verbose_json")
            .text("temperature", "0.0");
        let resp = self
            .client
            .post(format!("{}/inference", self.base_url))
            .multipart(form)
            .send()
            .await
            .map_err(|_| HostError::ChildDown)?;
        if !resp.status().is_success() {
            return Err(HostError::ChildDown);
        }
        let body: serde_json::Value = resp.json().await.map_err(|e| HostError::Http(e.to_string()))?;
        let duration_ms = started.elapsed().as_millis() as u32;
        parse_transcription(&body, duration_ms)
    }

    /// Is the child alive and reporting the model resident? whisper.cpp loads the
    /// model before it binds the port, so a successful HTTP response ≈ ready
    /// [VERIFY: prefer an explicit `/health` if the target build exposes one].
    pub async fn is_ready(&self) -> bool {
        self.client
            .get(&self.base_url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok()
    }

    /// Explicitly kill the child (also happens on `Drop` via `kill_on_drop`). The
    /// *guaranteed* release path is the parent killing this host's PID.
    pub async fn kill(&self) -> Result<(), HostError> {
        self.child
            .lock()
            .await
            .kill()
            .await
            .map_err(|e| HostError::Spawn(e.to_string()))
    }
}

/// Minimal RIFF/WAVE magic check — a decoded STT job must carry a real WAV
/// (doc 07 §2). Pure; guards [`WhisperChild::transcribe`] against garbage input.
fn is_wav(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

/// Parse a whisper server response into the contract shape (doc 07 §3). Reads the
/// `text` transcript and derives `avg_token_confidence` from segment stats:
/// prefer `avg_logprob` (→ `exp`, the per-segment probability), else fall back to
/// `1 − no_speech_prob`, else a neutral 1.0. Pure + tested — the wire shape itself
/// is [VERIFY] on-target, but the mapping is fixed here.
fn parse_transcription(body: &serde_json::Value, duration_ms: u32) -> Result<TranscribeResponse, HostError> {
    let transcript = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(TranscribeResponse {
        transcript,
        avg_token_confidence: confidence_from_body(body),
        duration_ms,
    })
}

/// Derive a mean per-segment confidence in `[0,1]` from a whisper verbose_json body.
fn confidence_from_body(body: &serde_json::Value) -> f32 {
    let Some(segs) = body.get("segments").and_then(|v| v.as_array()) else {
        return 1.0;
    };
    let logprobs: Vec<f64> = segs
        .iter()
        .filter_map(|s| s.get("avg_logprob").and_then(|v| v.as_f64()))
        .collect();
    if !logprobs.is_empty() {
        let mean = logprobs.iter().sum::<f64>() / logprobs.len() as f64;
        return (mean.exp() as f32).clamp(0.0, 1.0);
    }
    let no_speech: Vec<f64> = segs
        .iter()
        .filter_map(|s| s.get("no_speech_prob").and_then(|v| v.as_f64()))
        .collect();
    if !no_speech.is_empty() {
        let mean = no_speech.iter().sum::<f64>() / no_speech.len() as f64;
        return (1.0 - mean).clamp(0.0, 1.0) as f32;
    }
    1.0
}

/// Shared axum state: the supervised child.
type AppState = Arc<WhisperChild>;

async fn transcribe_handler(
    State(child): State<AppState>,
    Json(req): Json<TranscribeRequest>,
) -> Result<Json<TranscribeResponse>, StatusCode> {
    match child.transcribe(&req).await {
        Ok(resp) => Ok(Json(resp)),
        Err(HostError::BadAudio) => Err(StatusCode::UNPROCESSABLE_ENTITY),
        Err(HostError::Deadline) => Err(StatusCode::REQUEST_TIMEOUT),
        Err(_) => Err(StatusCode::BAD_GATEWAY),
    }
}

async fn health_handler(State(child): State<AppState>) -> Json<Health> {
    Json(Health {
        ready: child.is_ready().await,
        model: child.model_label.clone(),
        on_gpu: child.on_gpu,
    })
}

/// Entry point. Parse args, spawn + supervise the child, serve loopback HTTP, and
/// kill the child on shutdown so VRAM is released (doc 02 §2, doc 12 §6 step 4).
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // VRAM loads here (the child spawns + becomes ready).
    let child = Arc::new(WhisperChild::spawn(&args).await?);

    // Invariant 2: bind loopback ONLY. This host is not the gateway; it must never
    // be reachable off-box and never opens an outbound public socket (doc 13 §2).
    let app = Router::new()
        .route("/transcribe", post(transcribe_handler))
        .route("/health", get(health_handler))
        .with_state(Arc::clone(&child));
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, args.port)).await?;
    tracing::info!("aperture-stt-host listening on 127.0.0.1:{}", args.port);

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    // On shutdown: kill the child (belt-and-braces alongside kill_on_drop).
    let _ = child.kill().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_wav_accepts_riff_wave_and_rejects_junk() {
        let mut good = Vec::from(*b"RIFF");
        good.extend_from_slice(&[0, 0, 0, 0]);
        good.extend_from_slice(b"WAVE");
        assert!(is_wav(&good));
        assert!(!is_wav(b"not a wav"));
        assert!(!is_wav(&[]));
    }

    #[test]
    fn parse_transcription_reads_text_and_derives_confidence_from_logprob() {
        let body = serde_json::json!({
            "text": "  continue the rust tutorial  ",
            "segments": [ { "avg_logprob": -0.1 }, { "avg_logprob": -0.3 } ]
        });
        let r = parse_transcription(&body, 640).unwrap();
        assert_eq!(r.transcript, "continue the rust tutorial", "trimmed transcript");
        assert_eq!(r.duration_ms, 640);
        // exp(mean(-0.1,-0.3)) = exp(-0.2) ≈ 0.818.
        assert!((r.avg_token_confidence - 0.818f32).abs() < 0.01, "got {}", r.avg_token_confidence);
    }

    #[test]
    fn confidence_falls_back_to_no_speech_prob_then_neutral() {
        let ns = serde_json::json!({ "text": "x", "segments": [ { "no_speech_prob": 0.25 } ] });
        assert!((confidence_from_body(&ns) - 0.75).abs() < 1e-6);
        // No segments at all → neutral 1.0.
        let bare = serde_json::json!({ "text": "x" });
        assert_eq!(confidence_from_body(&bare), 1.0);
    }

    #[test]
    fn missing_text_is_an_empty_transcript_not_an_error() {
        let r = parse_transcription(&serde_json::json!({}), 10).unwrap();
        assert!(r.transcript.is_empty());
    }
}
