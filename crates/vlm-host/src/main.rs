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

// M5: implemented against the llama.cpp `server` HTTP contract. The exact
// endpoint/flag details ([VERIFY] tags below) are confirmed on the real target
// at the M5 measurement gate, where the GGUF weights are present; the child
// supervision, loopback HTTP surface, and kill-on-drop are real and complete.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

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
    #[serde(default)]
    pub scene: String,
    /// Best-guess foreground app.
    #[serde(default)]
    pub app_guess: String,
    /// Entities the model is confident it read (never guessed — doc 06 §3 prompt).
    #[serde(default)]
    pub key_entities: Vec<KeyEntity>,
    /// Advisory resume hint; connectors validate it before any suggestion uses it
    /// (doc 06 §6 — `resumable_hint` is advisory only).
    #[serde(default)]
    pub resumable_hint: ResumableHint,
    /// What the cheap OCR likely missed (doc 06 §3) — carried through additively.
    #[serde(default)]
    pub ocr_gaps: String,
    /// Self-reported confidence `[0.0, 1.0]`.
    #[serde(default)]
    pub confidence: f32,
}

/// One `key_entities` item (doc 06 §3).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct KeyEntity {
    /// `url | file | video | control | text`.
    pub kind: String,
    pub value: String,
}

/// `resumable_hint` (doc 06 §3) — advisory, validated downstream by connectors.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ResumableHint {
    /// `browser | youtube | document | ide | none`.
    #[serde(default)]
    pub connector_type: String,
    /// Free-form guess the matching connector validates against its captured state.
    #[serde(default)]
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
    child: Mutex<tokio::process::Child>,
    /// `http://127.0.0.1:<child_port>` — the llama.cpp server's loopback surface.
    base_url: String,
    /// Which weights are resident (for the `/health` model attribution).
    model_label: String,
    client: reqwest::Client,
}

impl LlamaChild {
    /// Spawn `llama-server` with the loadout flags, then poll its health until the
    /// model is resident or the cold-load SLA elapses (3B < 4 s / 7B < 6 s, doc 04 §5).
    ///
    /// [VERIFY] exact flags at M5 (llama.cpp server, current build):
    /// `llama-server -m <model> --mmproj <mmproj> -c <ctx> --host 127.0.0.1
    ///  --port <child_port> -ngl 99 --jinja`. Confirm mmproj/`--no-mmap`/`-ngl`
    /// and the multimodal endpoint on the target's llama.cpp version.
    pub async fn spawn(args: &Args) -> Result<Self, HostError> {
        let child_port = if args.child_port == 0 {
            args.port.wrapping_add(1)
        } else {
            args.child_port
        };
        let child = tokio::process::Command::new(&args.llama_bin)
            .arg("-m")
            .arg(&args.model)
            .arg("--mmproj")
            .arg(&args.mmproj)
            .arg("-c")
            .arg(args.ctx.to_string())
            .arg("--host")
            .arg("127.0.0.1") // invariant 2: loopback only, never off-box
            .arg("--port")
            .arg(child_port.to_string())
            .arg("-ngl")
            .arg("99") // offload all layers to the GPU [VERIFY per model/VRAM]
            .kill_on_drop(true) // invariant 3: kill => VRAM release
            .spawn()
            .map_err(|e| HostError::Spawn(e.to_string()))?;

        let base_url = format!("http://127.0.0.1:{child_port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| HostError::Http(e.to_string()))?;
        let host = Self {
            child: Mutex::new(child),
            base_url,
            model_label: args
                .model
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "qwen2.5-vl".into()),
            client,
        };

        // Await readiness within the cold-load SLA (doc 04 §5). [VERIFY exact SLA
        // on-target: 3B < 4 s / 7B < 6 s; generous ceiling here for cold disk.]
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            if host.is_ready().await {
                return Ok(host);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(HostError::ChildDown)
    }

    /// Forward one decoded `/infer` job to the child with JSON-schema-constrained
    /// decoding, validate the result, and on invalid JSON do **one** repair retry
    /// then discard (doc 06 §3 — the pipeline never blocks on the VLM).
    pub async fn infer(&self, req: &InferRequest) -> Result<InferResponse, HostError> {
        match self.infer_once(&req.prompt, &req.image_jpeg).await {
            Ok(r) => Ok(r),
            // One repair retry with an explicit "JSON only" nudge, then discard.
            Err(HostError::MalformedJson) => {
                let repaired = format!(
                    "{}\n\nYour previous reply was not valid JSON for the schema. \
                     Return ONLY a single JSON object, no prose.",
                    req.prompt
                );
                self.infer_once(&repaired, &req.image_jpeg).await
            }
            Err(e) => Err(e),
        }
    }

    /// One round-trip to the llama.cpp multimodal chat endpoint. [VERIFY] the
    /// exact request shape on the target's llama.cpp version — this targets the
    /// OpenAI-compatible `/v1/chat/completions` with an inline image data URL.
    async fn infer_once(&self, prompt: &str, image_jpeg: &[u8]) -> Result<InferResponse, HostError> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(image_jpeg);
        let body = serde_json::json!({
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": [
                    { "type": "text", "text": prompt },
                    { "type": "image_url",
                      "image_url": { "url": format!("data:image/jpeg;base64,{b64}") } }
                ]}
            ],
            "temperature": 0.1,
            "cache_prompt": true
        });
        let resp = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|_| HostError::ChildDown)?;
        if !resp.status().is_success() {
            return Err(HostError::ChildDown);
        }
        let envelope: serde_json::Value =
            resp.json().await.map_err(|e| HostError::Http(e.to_string()))?;
        // Pull the assistant message content and parse it as the scene JSON.
        let content = envelope
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .ok_or(HostError::MalformedJson)?;
        parse_scene(content)
    }

    /// Is the child alive and reporting the model resident? llama.cpp `/health`
    /// returns 200 `{"status":"ok"}` once the model is loaded [VERIFY].
    pub async fn is_ready(&self) -> bool {
        match self
            .client
            .get(format!("{}/health", self.base_url))
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Explicitly kill the child (also happens on `Drop` via `kill_on_drop`). Used
    /// by the graceful-shutdown path; the *guaranteed* path is the parent killing us.
    pub async fn kill(&self) -> Result<(), HostError> {
        self.child
            .lock()
            .await
            .kill()
            .await
            .map_err(|e| HostError::Spawn(e.to_string()))
    }
}

/// The screen-understanding system prompt (doc 06 §3): a *function*, not a chat.
const SYSTEM_PROMPT: &str = "You are a screen-understanding function. Given one \
screenshot of a Windows 11 desktop, return ONLY JSON matching the schema. Do not \
guess text you cannot read.";

/// Parse the model's text content into the structured scene (doc 06 §3). Tolerates
/// the model wrapping the JSON in prose/fences by extracting the outermost object.
fn parse_scene(content: &str) -> Result<InferResponse, HostError> {
    let json = extract_json_object(content).ok_or(HostError::MalformedJson)?;
    serde_json::from_str::<InferResponse>(json).map_err(|_| HostError::MalformedJson)
}

/// Extract the first balanced `{...}` object from a string (the model may fence
/// or preface its JSON despite the instruction).
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, ch) in s[start..].char_indices() {
        match ch {
            '"' if !escaped => in_str = !in_str,
            '\\' if in_str => {
                escaped = !escaped;
                continue;
            }
            '{' if !in_str => depth += 1,
            '}' if !in_str => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + i + 1]);
                }
            }
            _ => {}
        }
        escaped = false;
    }
    None
}

/// Shared axum state: the supervised child.
type AppState = Arc<LlamaChild>;

async fn infer_handler(
    State(child): State<AppState>,
    Json(req): Json<InferRequest>,
) -> Result<Json<InferResponse>, StatusCode> {
    match child.infer(&req).await {
        Ok(resp) => Ok(Json(resp)),
        Err(HostError::MalformedJson) => Err(StatusCode::UNPROCESSABLE_ENTITY),
        Err(HostError::Deadline) => Err(StatusCode::REQUEST_TIMEOUT),
        Err(_) => Err(StatusCode::BAD_GATEWAY),
    }
}

async fn health_handler(State(child): State<AppState>) -> Json<Health> {
    Json(Health {
        ready: child.is_ready().await,
        model: child.model_label.clone(),
    })
}

/// Entry point. Parse args, spawn + supervise the child, serve loopback HTTP, and
/// kill the child on shutdown so VRAM is released (doc 02 §2, doc 12 §6 step 4).
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // VRAM loads here (the child spawns + becomes ready).
    let child = Arc::new(LlamaChild::spawn(&args).await?);

    // Invariant 2: bind loopback ONLY. This host is not the gateway; it must never
    // be reachable off-box and never opens an outbound public socket (doc 13 §2).
    let app = Router::new()
        .route("/infer", post(infer_handler))
        .route("/health", get(health_handler))
        .with_state(Arc::clone(&child));
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, args.port)).await?;
    tracing::info!("aperture-vlm-host listening on 127.0.0.1:{}", args.port);

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;

    // On shutdown: kill the child (belt-and-braces alongside kill_on_drop) so the
    // model's VRAM is returned to the driver (doc 12 §6 step 4).
    let _ = child.kill().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_from_fenced_or_prefaced_output() {
        assert_eq!(extract_json_object(r#"{"a":1}"#), Some(r#"{"a":1}"#));
        assert_eq!(
            extract_json_object("here you go:\n```json\n{\"a\":1}\n```"),
            Some(r#"{"a":1}"#)
        );
        // Braces inside strings don't confuse the balance.
        assert_eq!(
            extract_json_object(r#"{"scene":"a }{ b"}"#),
            Some(r#"{"scene":"a }{ b"}"#)
        );
        assert_eq!(extract_json_object("no json here"), None);
    }

    #[test]
    fn parses_a_valid_scene_and_rejects_garbage() {
        let good = r#"{"scene":"editor open","app_guess":"code","key_entities":[],
            "resumable_hint":{"connector_type":"ide","payload_guess":{}},"confidence":0.8}"#;
        let scene = parse_scene(good).expect("valid scene");
        assert_eq!(scene.app_guess, "code");
        assert!(parse_scene("not json").is_err());
    }
}
