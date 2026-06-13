//! Voice / push-to-talk STT subsystem (doc 07).
//!
//! One press-and-hold gesture drives the whole pipeline:
//! ```text
//! PTT down ─► WASAPI capture (16 kHz mono PCM) ─► Silero VAD trim
//!          ─► STT GpuJob{kind:Stt, priority:100} ─► transcript+confidence
//!          ─► voice_utterance store + embed (ALWAYS — telemetry role, locked decision B)
//!          ─► deterministic intent classify
//!               • query      ─► retrieval ─► answer bubble (doc 11)
//!               • escalation ─► payload draft ─► transparency gate (doc 09) — NEVER auto-send
//!               • telemetry  ─► stored only, no UI
//! ```
//!
//! ## Invariants honored here
//! - **VRAM ceiling / single GPU mutex (doc 04, doc 12):** STT runs *only* as a
//!   [`aperture_contracts::GpuJob`] enqueued on the orchestration crate's
//!   [`GpuScheduler`]. This crate never touches the GPU, a sidecar, or VRAM
//!   accounting. Whisper-small CPU fallback is the orchestrator's decision; we
//!   simply submit the job (doc 07 §3).
//! - **Two-emitter transparency gate (doc 13 §2):** voice escalation (`"ask claude …"`)
//!   only *assembles* a [`ContextPayload`] and hands it to the preview→Send gate.
//!   This crate opens **no** network socket and spawns **no** Claude CLI — only the
//!   reasoning-gateway crate may (doc 07 §5).
//! - **Capture toggle (doc 12 §6):** when capture is OFF the hotkey is unregistered
//!   and no mic stream is opened; PTT is inert.
//!
//! TODO(M6:) wire this facade end-to-end; M0..M5 land the deps it leans on.

pub mod audio_capture;
pub mod hotkey;
pub mod intent_classifier;
pub mod retrieval;
pub mod stt_job;
pub mod utterance_logger;
pub mod vad;

use std::sync::Arc;
use std::time::Duration;

use aperture_contracts::gpu_job::GpuScheduler;

pub use intent_classifier::{Intent as VoiceIntent, IntentResult};
pub use retrieval::AnswerBubble;

/// Max single utterance (doc 07 §2) — key-up or this ceiling ends capture.
pub const MAX_UTTERANCE: Duration = Duration::from_secs(30);

/// Errors surfaced by the voice subsystem facade.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    /// `RegisterHotKey` failed — usually a conflict with another app (doc 07 §6);
    /// the shell surfaces a rebind prompt.
    #[error("hotkey registration failed: {0}")]
    HotkeyConflict(String),
    /// Default mic unavailable or OS mic permission denied (doc 07 §2): PTT is
    /// disabled with a one-time explanatory notice.
    #[error("microphone unavailable: {0}")]
    MicUnavailable(String),
    /// The STT GpuJob failed on both GPU and CPU fallback (doc 07 §3, §6).
    #[error("transcription failed: {0}")]
    Stt(String),
    /// Storage / embedding error from the unconditional telemetry write (doc 07 §3).
    #[error("utterance logging failed: {0}")]
    Logging(String),
    /// Retrieval over history failed (doc 07 §5).
    #[error("retrieval failed: {0}")]
    Retrieval(String),
}

/// Outcome of one completed PTT gesture, handed up to the shell (doc 07 §4-§5).
#[derive(Debug)]
pub enum UtteranceOutcome {
    /// Sub-300 ms of speech after VAD trim — discarded as an accidental tap
    /// (doc 07 §2). Still nothing is stored (no real utterance occurred).
    DiscardedTap,
    /// Confidence < 0.6: show a transcript chip ("Did you say: …?") with Run/Dismiss;
    /// never act on a guess (doc 07 §4.4). Always stored + embedded first.
    ConfirmChip { transcript: String },
    /// Query intent resolved to an answer bubble (doc 07 §5).
    Answer(AnswerBubble),
    /// `"ask claude …"` escalation: a payload draft is ready for the preview→Send
    /// gate. NEVER auto-sent (doc 07 §4.2, §5).
    EscalationDraft {
        transcript: String,
        // TODO(M7:) carry the assembled `aperture_contracts::ContextPayload` here
        //           once the payload builder lands; the gateway owns the send.
    },
    /// Telemetry-only utterance: stored + embedded, no UI (doc 07 §4.3).
    StoredSilently,
}

/// The voice subsystem facade the Tauri shell drives. It owns the hotkey and mic,
/// and orchestrates the per-utterance pipeline; the GPU is reached *only* via the
/// injected [`GpuScheduler`] (doc 12 §1, doc 15 §4).
pub struct VoiceSubsystem {
    /// The single GPU entry point (orchestration crate's implementor). STT jobs go
    /// here at priority 100; never constructed by this crate (doc 15 §4).
    _scheduler: Arc<dyn GpuScheduler>,
    // _db: aperture_db::Db handle for events/ctx_vec/connector_state (doc 03).
    // _embedder: aperture_embedding handle (768-d nomic-embed, doc 03 §5).
    // _hotkey: hotkey::PttHotkey,
    // _config: VoiceConfig (hotkey chord, model choice, score floor — from settings).
}

impl VoiceSubsystem {
    /// Construct the subsystem with its injected GPU scheduler. Registering the
    /// hotkey and opening the mic happen on [`enable`](Self::enable) so the capture
    /// toggle can flip voice on/off cheaply (doc 12 §6).
    pub fn new(scheduler: Arc<dyn GpuScheduler>) -> Self {
        // TODO(M6:) also take a Db handle, an embedder, and VoiceConfig.
        Self {
            _scheduler: scheduler,
        }
    }

    /// Capture toggle ON: register the global PTT hotkey (doc 07 §2). Mic-permission
    /// denial surfaces [`VoiceError::MicUnavailable`] (one-time notice, doc 07 §2).
    pub fn enable(&mut self) -> Result<(), VoiceError> {
        // TODO(M6:) hotkey::PttHotkey::register(self.config.chord) + a probe open
        //           of the default mic to detect a denied permission early.
        todo!("M6: register PTT hotkey + probe default mic")
    }

    /// Capture toggle OFF: unregister the hotkey and drop any live mic stream so
    /// PTT goes inert in <3 s (doc 12 §6). Sidecar/VRAM teardown is the
    /// orchestrator's job, not ours.
    pub fn disable(&mut self) {
        // TODO(M6:) drop hotkey registration + abort any in-flight capture.
        todo!("M6: unregister hotkey + release mic")
    }

    /// Key-down: start WASAPI capture into a 16 kHz mono PCM buffer; the shell
    /// shows the "listening" pill (doc 07 §2, doc 11). Capture auto-stops at
    /// [`MAX_UTTERANCE`].
    pub fn ptt_down(&mut self) -> Result<(), VoiceError> {
        // TODO(M6:) audio_capture::Recorder::start(); arm the 30 s ceiling timer.
        todo!("M6: begin capture (default mic, 16 kHz mono); start 30 s ceiling")
    }

    /// Key-up: stop capture, then run the full per-utterance pipeline (doc 07 §2-§5).
    ///
    /// Order is load-bearing: **store + embed happen before any intent branch**, so
    /// the telemetry role is truly unconditional (locked decision B, doc 07 §3).
    pub async fn ptt_up(&mut self) -> Result<UtteranceOutcome, VoiceError> {
        // TODO(M6:) pipeline:
        //   1. pcm = recorder.stop();
        //   2. trimmed = vad::trim(&pcm); if trimmed.speech_ms < 300 => DiscardedTap.
        //   3. job = stt_job::build(trimmed.to_wav()); out = scheduler.enqueue(job).await
        //          (priority 100, never cancellable; orchestrator does CPU fallback).
        //   4. utterance_logger::store_and_embed(...)  // ALWAYS, before any branch.
        //   5. match intent_classifier::classify(&transcript, confidence) {
        //          confidence < 0.6        => ConfirmChip,
        //          Escalation              => assemble payload draft -> EscalationDraft,
        //          Query                   => retrieval::run(...).await -> Answer,
        //          Telemetry               => StoredSilently,
        //      }
        todo!("M6: stop capture -> VAD -> STT job -> store+embed -> intent -> branch")
    }
}
