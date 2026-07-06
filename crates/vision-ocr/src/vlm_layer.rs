//! Layer B — on-demand VLM scene understanding (doc 06 §3, M5).
//!
//! Model: Qwen2.5-VL (3B default / 7B opt-in, doc 04 §3) served by the
//! `vlm-host` sidecar. This crate **never** talks to the sidecar or the GPU
//! directly — it builds one [`GpuJob`](aperture_contracts::gpu_job::GpuJob) and
//! hands it to the orchestration-owned
//! [`GpuScheduler`](aperture_contracts::gpu_job::GpuScheduler) (the single GPU
//! mutex + projection check live there; doc 12 §3/§4). That is how the 8 GB
//! ceiling is enforced from one place.
//!
//! ## Hard invariants (doc 06 §3, doc 02 Path A)
//! - **Never blocks a bubble.** The result only enriches
//!   `screen_context.vlm_summary` and improves the *next* pattern cycle. A
//!   caller must `tokio::spawn` this off the bubble path; it returns nothing the
//!   UI is allowed to wait on.
//! - **One image per job**, downscaled ≤ 1024 px long edge, JPEG q85 (doc 06 §3 /
//!   gpu_job.rs `GpuJobKind::Vlm` — "image prefill is the silent killer", R2).
//! - **One repair retry, then discard.** Schema-invalid JSON ⇒ one re-ask with a
//!   repair instruction; still invalid ⇒ drop (the pipeline never blocks on the
//!   VLM, doc 06 §3).

use std::sync::Arc;

use aperture_contracts::gpu_job::{priority, GpuJob, GpuJobKind, GpuScheduler, JobOutput};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::VisionError;

/// VLM job deadline (doc 06 §6 / doc 12 §3) [ASSUMPTION]: cold-load slow ⇒ on
/// timeout cancel + log, never retried in a loop.
pub const VLM_DEADLINE: Duration = Duration::from_secs(10);

/// Max long edge for the image handed to the sidecar (doc 06 §3); enforces the
/// OOM rule R2.
pub const VLM_MAX_LONG_EDGE_PX: u32 = 1024;

/// JPEG quality for the single VLM image (doc 06 §3).
pub const VLM_JPEG_QUALITY: u8 = 85;

/// The system prompt — a screen-understanding *function*, not a chat (doc 06 §3).
pub const VLM_SYSTEM_PROMPT: &str = "You are a screen-understanding function. \
Given one screenshot of a Windows 11 desktop, return ONLY JSON matching the schema. \
Do not guess text you cannot read.";

/// Appended on the single repair attempt when the first response failed the
/// schema (doc 06 §3).
pub const VLM_REPAIR_INSTRUCTION: &str =
    "Your previous response was not valid JSON for the required schema. \
Return ONLY a single JSON object matching the schema, with no prose.";

/// The structured scene the VLM must return (doc 06 §3). Deserialized from the
/// sidecar's [`JobOutput::Vlm`] JSON; `resumable_hint` is **advisory only** —
/// connectors validate against their own captured state before any suggestion
/// uses it (doc 06 §6, RK hallucination).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneJson {
    /// Short scene description.
    pub scene: String,
    /// The VLM's guess at the foreground app.
    pub app_guess: String,
    /// Salient entities on screen.
    pub key_entities: Vec<KeyEntity>,
    /// Advisory connector hint — never trusted directly (doc 06 §6).
    pub resumable_hint: ResumableHint,
    /// What the cheap OCR likely missed (doc 06 §3).
    pub ocr_gaps: String,
    /// Self-reported confidence in `[0.0, 1.0]`.
    pub confidence: f32,
}

/// One entity the VLM extracted from the frame (doc 06 §3 schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEntity {
    /// `url | file | video | control | text` (doc 06 §3). Kept as a string for
    /// additive tolerance of unknown kinds (doc 15 §6).
    pub kind: String,
    pub value: String,
}

/// Advisory resume hint (doc 06 §3). `payload_guess` is free-form JSON the
/// owning connector must validate before use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumableHint {
    /// `browser | youtube | document | ide | none` (doc 06 §3).
    pub connector_type: String,
    #[serde(default)]
    pub payload_guess: serde_json::Value,
}

/// Builds VLM jobs and parses their results. Holds a handle to the
/// orchestration scheduler — the *only* sanctioned path to the GPU.
pub struct VlmLayer {
    scheduler: Arc<dyn GpuScheduler>,
}

impl VlmLayer {
    /// Wire the layer to the orchestration-owned scheduler (doc 12). In tests,
    /// pass `aperture_contracts::fakes::FakeScheduler` to exercise the
    /// budget-refused / sidecar-down degrade paths without a GPU.
    pub fn new(scheduler: Arc<dyn GpuScheduler>) -> Self {
        Self { scheduler }
    }

    /// Run scene understanding on one already-downscaled, JPEG-q85 image
    /// (≤ 1024 px long edge — see [`prepare_image`]). Builds a `prio:50`
    /// pattern-VLM job (cancellable; doc 12 §3), enqueues it, then parses +
    /// (once) repairs the JSON.
    ///
    /// Returns:
    /// - `Ok(SceneJson)` on first-pass or post-repair success;
    /// - `Err(VisionError::VlmUnusable)` if still schema-invalid after the one
    ///   repair, or if the scheduler refused / the job timed out — **all soft**:
    ///   the caller proceeds OCR-only and the bubble is unaffected (doc 06 §6).
    ///
    /// MUST be called off the bubble path (e.g. inside `tokio::spawn`).
    pub async fn understand(&self, image_jpeg: Vec<u8>) -> Result<SceneJson, VisionError> {
        // First pass (prio:50 pattern-VLM, cancellable — doc 12 §3).
        match self.scheduler.enqueue(self.build_job(image_jpeg.clone(), false)).await {
            Ok(output) => match parse_scene(output) {
                Ok(scene) => Ok(scene),
                // Schema-invalid: ONE repair retry with an explicit nudge, then
                // discard (doc 06 §3 — the pipeline never blocks on the VLM).
                Err(_) => match self.scheduler.enqueue(self.build_job(image_jpeg, true)).await {
                    Ok(output) => parse_scene(output),
                    Err(e) => {
                        tracing::debug!(?e, "VLM repair enqueue refused; OCR-only (doc 06 §6)");
                        Err(VisionError::VlmUnusable)
                    }
                },
            },
            // BudgetRefused / Deadline / Cancelled / SidecarDown are all soft:
            // skip the wake, proceed OCR-only (doc 06 §6). VLM never gates a bubble.
            Err(e) => {
                tracing::debug!(?e, "VLM job not run; OCR-only (doc 06 §6)");
                Err(VisionError::VlmUnusable)
            }
        }
    }

    /// Construct the GPU job for one image (doc 06 §3 / gpu_job.rs).
    /// `repair` switches the prompt to the repair instruction for the single retry.
    fn build_job(&self, image_jpeg: Vec<u8>, repair: bool) -> GpuJob {
        let prompt = if repair {
            format!("{VLM_SYSTEM_PROMPT}\n\n{VLM_REPAIR_INSTRUCTION}")
        } else {
            VLM_SYSTEM_PROMPT.to_string()
        };
        // pattern-VLM is always priority::VLM_PATTERN and cancellable (doc 12 §3).
        GpuJob {
            kind: GpuJobKind::Vlm {
                image_jpeg,
                prompt,
            },
            priority: priority::VLM_PATTERN,
            deadline: VLM_DEADLINE,
        }
    }
}

/// Parse + validate a sidecar `JobOutput::Vlm` value against the scene schema
/// (doc 06 §3). Returns `VlmUnusable` on any schema violation so the caller can
/// decide whether to repair or discard. Clamps confidence to `[0,1]`; a scene
/// with no description is treated as unusable (the model returned nothing useful).
pub fn parse_scene(output: JobOutput) -> Result<SceneJson, VisionError> {
    let JobOutput::Vlm(value) = output else {
        return Err(VisionError::VlmUnusable);
    };
    let mut scene: SceneJson =
        serde_json::from_value(value).map_err(|_| VisionError::VlmUnusable)?;
    if scene.scene.trim().is_empty() {
        return Err(VisionError::VlmUnusable);
    }
    scene.confidence = scene.confidence.clamp(0.0, 1.0);
    Ok(scene)
}

/// A valid scene JSON body for tests + the doc 06 §3 golden shape.
#[cfg(test)]
const GOLDEN_SCENE: &str = r#"{"scene":"a code editor with a terminal",
    "app_guess":"code","key_entities":[{"kind":"file","value":"main.rs"}],
    "resumable_hint":{"connector_type":"ide","payload_guess":{}},
    "ocr_gaps":"minimap text","confidence":0.82}"#;

/// Downscale to ≤ 1024 px long edge and re-encode JPEG q85 for the sidecar
/// (doc 06 §3, R2 — image prefill is the silent killer). One image only. Input
/// is raw **BGRA8** bytes (the sampled frame format) plus its dimensions.
pub fn prepare_image(bgra: &[u8], width: u32, height: u32) -> Result<Vec<u8>, VisionError> {
    use image::{ImageBuffer, Rgba};

    if width == 0 || height == 0 || bgra.len() < (width as usize * height as usize * 4) {
        return Err(VisionError::Image("empty or truncated frame".into()));
    }
    // BGRA -> RGBA (the `image` crate is RGBA-native).
    let mut rgba = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    let buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_raw(width, height, rgba)
        .ok_or_else(|| VisionError::Image("buffer size mismatch".into()))?;
    let mut dynimg = image::DynamicImage::ImageRgba8(buf);

    // Downscale to the long-edge cap (only ever shrinks — doc 06 §3).
    let long_edge = width.max(height);
    if long_edge > VLM_MAX_LONG_EDGE_PX {
        let scale = VLM_MAX_LONG_EDGE_PX as f32 / long_edge as f32;
        let (nw, nh) = (
            (width as f32 * scale).round().max(1.0) as u32,
            (height as f32 * scale).round().max(1.0) as u32,
        );
        dynimg = dynimg.resize(nw, nh, image::imageops::FilterType::Triangle);
    }

    // Encode JPEG q85 (RGB — JPEG has no alpha).
    let rgb = dynimg.to_rgb8();
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, VLM_JPEG_QUALITY)
        .encode_image(&image::DynamicImage::ImageRgb8(rgb))
        .map_err(|e| VisionError::Image(e.to_string()))?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::gpu_job::JobError;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn scene_output(json: &str) -> JobOutput {
        JobOutput::Vlm(serde_json::from_str(json).unwrap())
    }

    #[test]
    fn parse_scene_accepts_the_golden_shape_and_clamps_confidence() {
        let scene = parse_scene(scene_output(GOLDEN_SCENE)).expect("valid");
        assert_eq!(scene.app_guess, "code");
        assert_eq!(scene.resumable_hint.connector_type, "ide");
        // Out-of-range confidence is clamped.
        let hot = scene_output(r#"{"scene":"x","app_guess":"a","key_entities":[],
            "resumable_hint":{"connector_type":"none","payload_guess":{}},
            "ocr_gaps":"","confidence":9.0}"#);
        assert_eq!(parse_scene(hot).unwrap().confidence, 1.0);
    }

    #[test]
    fn parse_scene_rejects_empty_or_wrong_shape() {
        assert!(parse_scene(scene_output(r#"{"scene":"  ","app_guess":"a","key_entities":[],
            "resumable_hint":{"connector_type":"none","payload_guess":{}},"confidence":0.5}"#))
            .is_err());
        assert!(parse_scene(JobOutput::Stt {
            transcript: "x".into(),
            avg_token_confidence: 0.9,
            duration_ms: 10,
        })
        .is_err());
    }

    /// A scheduler whose Nth `enqueue` returns the Nth canned output — lets us
    /// drive the first-fail / repair-succeed path deterministically.
    struct SequencedScheduler {
        outputs: Vec<Result<JobOutput, JobError>>,
        calls: AtomicU32,
    }

    #[async_trait::async_trait]
    impl GpuScheduler for SequencedScheduler {
        async fn enqueue(&self, _job: GpuJob) -> Result<JobOutput, JobError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) as usize;
            self.outputs
                .get(n)
                .cloned()
                .unwrap_or(Err(JobError::SidecarDown))
        }
    }

    #[tokio::test]
    async fn understand_returns_the_scene_on_first_pass() {
        let sched = Arc::new(SequencedScheduler {
            outputs: vec![Ok(scene_output(GOLDEN_SCENE))],
            calls: AtomicU32::new(0),
        });
        let layer = VlmLayer::new(sched.clone());
        let scene = layer.understand(vec![0u8; 8]).await.expect("scene");
        assert_eq!(scene.app_guess, "code");
        assert_eq!(sched.calls.load(Ordering::SeqCst), 1, "no repair needed");
    }

    #[tokio::test]
    async fn understand_repairs_once_then_succeeds() {
        let sched = Arc::new(SequencedScheduler {
            outputs: vec![
                Ok(JobOutput::Vlm(serde_json::json!("not an object"))), // schema-invalid
                Ok(scene_output(GOLDEN_SCENE)),                          // repair succeeds
            ],
            calls: AtomicU32::new(0),
        });
        let layer = VlmLayer::new(sched.clone());
        let scene = layer.understand(vec![0u8; 8]).await.expect("repaired scene");
        assert_eq!(scene.app_guess, "code");
        assert_eq!(sched.calls.load(Ordering::SeqCst), 2, "exactly one repair retry");
    }

    #[tokio::test]
    async fn understand_degrades_to_ocr_only_on_budget_refusal() {
        let sched = Arc::new(SequencedScheduler {
            outputs: vec![Err(JobError::BudgetRefused { projection_gb: 8.4 })],
            calls: AtomicU32::new(0),
        });
        let layer = VlmLayer::new(sched);
        // Soft failure: the caller proceeds OCR-only (doc 06 §6). Never panics.
        assert!(matches!(
            layer.understand(vec![0u8; 8]).await,
            Err(VisionError::VlmUnusable)
        ));
    }

    #[test]
    fn prepare_image_downscales_and_encodes_jpeg() {
        // A 2000x1000 BGRA frame -> long edge capped at 1024, JPEG magic bytes.
        let (w, h) = (2000u32, 1000u32);
        let bgra = vec![128u8; (w * h * 4) as usize];
        let jpeg = prepare_image(&bgra, w, h).expect("encoded");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI marker");
        // Decode back to confirm the long edge is within the cap.
        let decoded = image::load_from_memory(&jpeg).expect("valid jpeg");
        assert!(decoded.width().max(decoded.height()) <= VLM_MAX_LONG_EDGE_PX);
        assert_eq!(decoded.width(), 1024);
    }

    #[test]
    fn prepare_image_rejects_truncated_frames() {
        assert!(matches!(
            prepare_image(&[0u8; 4], 100, 100),
            Err(VisionError::Image(_))
        ));
    }
}
