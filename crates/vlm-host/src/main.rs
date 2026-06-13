//! `aperture-vlm-host` — the VLM sidecar **binary** (doc 06 §3, doc 02 §2, doc 04 §3).
//!
//! This is a **standalone process**, not a library called in-process. The
//! Orchestration Resource Manager (doc 12 §5) spawns it on first VLM demand and
//! **kills** it to release VRAM. Process death is the only *guaranteed* way to
//! return VRAM to the driver (doc 02 §2); in-process bindings make unload
//! best-effort and would break the 8 GB ceiling and SC6 (< 3 s release on
//! capture-OFF). This binary therefore owns exactly one responsibility: launch,
//! supervise, and — on shutdown — **kill** a llama.cpp `server` child so that
//! killing *us* transitively reaps the GPU memory.
//!
//! Layout (this host) ──spawns──► llama.cpp `server` child (the model lives in VRAM):
//! ```text
//!   orchestration ──spawn/kill──► [aperture-vlm-host] ──spawn/kill──► llama.cpp server
//!         ▲ POST /infer, GET /health (loopback, --port)        ▲ loopback HTTP
//! ```
//!
//! ## Invariants honored here
//! - **(1) 8 GB VRAM ceiling / single-GPU mutex.** This host does *not* arbitrate
//!   the mutex (that is orchestration, doc 12 §3) — it just runs one model. But it
//!   enforces the doc 04 R2 input shape: exactly **one image**, downscaled
//!   ≤ 1024 px long edge, JPEG q85, ctx capped (4K in L2). One image per `/infer`.
//! - **(2) two-emitter transparency gate.** This binary opens sockets **only** on
//!   loopback (`127.0.0.1:--port`) to its own llama.cpp child. It is NOT the
//!   reasoning gateway: it never reaches the public internet and never spawns the
//!   Claude CLI (doc 13 §2). Bind address is hard-pinned to loopback below.
//! - **(3) capture toggle.** On `SIGTERM`/parent-kill we kill the child and exit;
//!   the guaranteed-release path is the parent simply killing *this* PID, which the
//!   OS-level child kill-on-drop backstops (doc 12 §6 step 4).
//!
//! Model selection (doc 04 §3 loadouts): **L1 = Qwen2.5-VL 3B + mmproj (default)**,
//! **L2 = Qwen2.5-VL 7B exclusive (opt-in)**. The *host* is loadout-agnostic — the
//! parent passes `--model`/`--mmproj`/`--ctx`; this binary just wires the flags.
//!
//! [VERIFY] exact llama.cpp `server` binary name, flags, and version at M5
//! (doc 04 §9 measurement plan; cold-load SLA 3B < 4 s / 7B < 6 s, doc 04 §5).

// TODO(M5): vlm/gpu milestone — implement the bodies below against the measured
// llama.cpp server contract; wire cold-load SLA timing into the M5 gate report.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use clap::Parser;
use serde::{Deserialize, Serialize};

/// CLI surface. The orchestration Manager (doc 12 §2 `ModelLifecycle`) supplies
/// these when it spawns the sidecar; nothing here is read from the environment so
/// the spawn is fully reproducible/auditable.
#[derive(Debug, Parser)]
#[command(name = "aperture-vlm-host", about = "Aperture VLM sidecar (Qwen2.5-VL via llama.cpp)")]
pub struct Args {
    /// Loopback port for this host's own HTTP surface (`/infer`, `/health`).
    /// The parent pins this; we always bind `127.0.0.1` only (invariant 2).
    #[arg(long)]
    pub port: u16,

    /// Path to the llama.cpp `server` executable. [VERIFY] name/flags at M5.
    #[arg(long, default_value = "llama-server")]
    pub llama_bin: PathBuf,

    /// GGUF weights. Default = Qwen2.5-VL **3B** Q4_K_M (L1, doc 04 §3).
    /// L2 opt-in passes the 7B path instead (run *exclusive*, doc 04 §3 rule).
    #[arg(long)]
    pub model: PathBuf,

    /// Vision projector (mmproj, FP16 — always FP16, doc 04 §2). Required for VL.
    #[arg(long)]
    pub mmproj: PathBuf,

    /// Context window cap. Doc 04 R2: cap at 4K tokens in L2 [ASSUMPTION].
    #[arg(long, default_value_t = 4096)]
    pub ctx: u32,

    /// Port the spawned llama.cpp child listens on (loopback). Distinct from `--port`.
    #[arg(long, default_value_t = 0)]
    pub child_port: u16,
}

/// `POST /infer` request body. One image only (doc 06 §3, doc 04 R2 — image
/// prefill is the silent killer). The bytes are already downscaled ≤ 1024 px long
/// edge, JPEG q85, by the caller (the GPU job contract, [`aperture_contracts::GpuJobKind::Vlm`]).
#[derive(Debug, Deserialize)]
pub struct InferRequest {
    /// Single downscaled JPEG frame (≤ 1024 px long edge, q85).
    pub image_jpeg: Vec<u8>,
    /// The screen-understanding prompt (doc 06 §3 system template applied here).
    pub prompt: String,
    /// JSON schema the model must match (doc 06 §3 structured-output schema). The
    /// host forwards this to llama.cpp's grammar/JSON-schema constrained decoding.
    pub schema: serde_json::Value,
}

/// `POST /infer` success body — the doc 06 §3 structured scene JSON. Mirrors the
/// shape that flattens into `aperture_contracts::JobOutput::Vlm(serde_json::Value)`
/// and lands in `screen_context.vlm_summary` (doc 06 §5). Kept as a typed struct
/// here for the host's own validate-or-repair step; serialized back as JSON.
#[derive(Debug, Serialize, Deserialize)]
pub struct InferResponse {
    /// Short scene description.
    pub scene: String,
    /// Best-guess foreground app.
    pub app_guess: String,
    /// Entities the model is confident it read (never guessed — doc 06 §3 prompt).
    pub key_entities: Vec<KeyEntity>,
    /// Advisory resume hint; connectors validate it before any suggestion uses it
    /// (doc 06 §6 — `resumable_hint` is advisory only).
    pub resumable_hint: ResumableHint,
    /// Self-reported confidence `[0.0, 1.0]`.
    pub confidence: f32,
}

/// One `key_entities` item (doc 06 §3).
#[derive(Debug, Serialize, Deserialize)]
pub struct KeyEntity {
    /// `url | file | video | control | text`.
    pub kind: String,
    pub value: String,
}

/// `resumable_hint` (doc 06 §3) — advisory, validated downstream by connectors.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResumableHint {
    /// `browser | youtube | document | ide | none`.
    pub connector_type: String,
    /// Free-form guess the matching connector validates against its captured state.
    pub payload_guess: serde_json::Value,
}

/// `GET /health` readiness body. The parent's `ModelLifecycle` polls this; readiness
/// = the llama.cpp child reports its model loaded (doc 12 §5 "readiness = health OK").
#[derive(Debug, Serialize)]
pub struct Health {
    /// `true` once the llama.cpp child has the model resident and is accepting jobs.
    pub ready: bool,
    /// Which weights are resident (for the M5 VRAM-attribution report, doc 04 §9).
    pub model: String,
}

/// Errors surfaced to the parent. A child crash maps to the orchestration
/// `JobError::SidecarDown` (doc 12 §5 restart-with-backoff, then degrade to OCR-only).
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("failed to spawn llama.cpp server child: {0}")]
    Spawn(String),
    #[error("llama.cpp child exited / is unreachable")]
    ChildDown,
    /// Doc 06 §3: invalid JSON ⇒ one repair retry, then discard (never block).
    #[error("model returned malformed JSON after one repair attempt")]
    MalformedJson,
    #[error("inference deadline exceeded")] // VLM 10 s (doc 12 §3)
    Deadline,
    #[error("http error: {0}")]
    Http(String),
}

/// Supervises the llama.cpp `server` child. Holds the child handle so that
/// dropping/killing **this** host transitively kills the child — the
/// guaranteed VRAM-release primitive (doc 02 §2, doc 12 §5).
pub struct LlamaChild {
    // child: tokio::process::Child,   // spawned with kill_on_drop(true)
    // base_url: String,               // http://127.0.0.1:<child_port>
}

impl LlamaChild {
    /// Spawn `llama-server` with the loadout flags, then poll its health until the
    /// model is resident or the cold-load SLA elapses (3B < 4 s / 7B < 6 s, doc 04 §5).
    ///
    /// [VERIFY] exact flags at M5, e.g. (illustrative, not final):
    /// `llama-server -m <model> --mmproj <mmproj> -c <ctx> --host 127.0.0.1
    ///  --port <child_port> -ngl 99 --jinja`. Confirm mmproj/`--no-mmap`/`-ngl`.
    pub async fn spawn(_args: &Args) -> Result<Self, HostError> {
        // TODO(M5):
        //   1. tokio::process::Command::new(args.llama_bin)
        //        .kill_on_drop(true)            // invariant 3: kill => VRAM release
        //        .args([...measured flags...]);
        //   2. await child readiness via its /health within the cold-load SLA.
        //   3. record load latency + nvidia-smi VRAM delta for the M5 gate (doc 04 §9).
        todo!("M5: spawn llama.cpp server child with kill_on_drop; await readiness")
    }

    /// Forward one decoded `/infer` job to the child with JSON-schema-constrained
    /// decoding, validate the result, and on invalid JSON do **one** repair retry
    /// then discard (doc 06 §3 — the pipeline never blocks on the VLM).
    pub async fn infer(&self, _req: &InferRequest) -> Result<InferResponse, HostError> {
        // TODO(M5): proxy to child /completion (or /v1/chat/completions) with the
        // single image + schema grammar; parse → InferResponse; one repair retry.
        todo!("M5: proxy to llama.cpp child, schema-constrained decode, 1 repair retry")
    }

    /// Is the child alive and reporting the model resident?
    pub async fn is_ready(&self) -> bool {
        // TODO(M5): GET child /health (or /props) and map to readiness.
        todo!("M5: probe llama.cpp child readiness")
    }

    /// Explicitly kill the child (also happens on `Drop` via `kill_on_drop`). Used
    /// by the graceful-shutdown path; the *guaranteed* path is the parent killing us.
    pub async fn kill(&mut self) -> Result<(), HostError> {
        // TODO(M5): child.kill().await; this is what returns VRAM to the driver.
        todo!("M5: kill llama.cpp child => VRAM returned")
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

    // TODO(M5):
    //   1. let child = LlamaChild::spawn(&args).await? ;  // VRAM loads here
    //   2. build axum::Router:
    //        POST /infer  -> decode InferRequest -> child.infer() -> InferResponse JSON
    //        GET  /health -> Health { ready: child.is_ready().await, model }
    //   3. axum::serve(TcpListener::bind(_bind), router)
    //        .with_graceful_shutdown(ctrl_c / SIGTERM)
    //        — on shutdown: child.kill().await  (then drop also kills, belt+braces).
    //   4. record cold-load + per-infer latency into the M5 gate report (doc 04 §9).
    let _ = args;
    todo!("M5: spawn child, serve loopback /infer + /health, kill child on shutdown")
}
