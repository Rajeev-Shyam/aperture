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
//!   accounting. The whisper.cpp base/tiny CPU fallback (ADR-024) is the
//!   orchestrator's decision; we simply submit the job (doc 07 §3).
//! - **Two-emitter transparency gate (doc 13 §2):** voice escalation (`"ask claude …"`)
//!   only *assembles* a [`ContextPayload`] and hands it to the preview→Send gate.
//!   This crate opens **no** network socket and spawns **no** Claude CLI — only the
//!   reasoning-gateway crate may (doc 07 §5).
//! - **Capture toggle (doc 12 §6):** when capture is OFF the hotkey is unregistered
//!   and no mic stream is opened; PTT is inert.
//!
//! The facade is wired end-to-end here (M6). The per-utterance pipeline lives in
//! [`VoiceSubsystem::process_utterance`], factored out of live mic capture so it is
//! testable with a synthetic PCM + `FakeScheduler` + in-memory DB; the hardware
//! I/O (`enable`/`disable`/`ptt_down`/`ptt_up`) is best-effort and **UNVERIFIED**.

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
use aperture_db::Db;
use aperture_embedding::Embedder;

pub use intent_classifier::{Intent as VoiceIntent, IntentResult};
pub use retrieval::AnswerBubble;

/// Voice subsystem settings (doc 07 §2-§3), sourced from the app settings by the
/// composition root. Kept small; retrieval's score floor stays [`retrieval::SCORE_FLOOR`].
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    /// The PTT chord (doc 07 §2), e.g. `Ctrl+Win+Space`.
    pub chord: hotkey::HotkeyChord,
    /// STT model label recorded on each utterance (doc 07 §3).
    pub stt_model: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            chord: hotkey::HotkeyChord::default(),
            stt_model: utterance_logger::DEFAULT_STT_MODEL.to_string(),
        }
    }
}

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
///
/// **Not `Send`** — it holds a `cpal::Stream` and a `GlobalHotKeyManager`, so the
/// composition root drives it on a dedicated OS thread with a current-thread
/// runtime (never across a multi-thread `tokio::spawn`). The `!Send` capture is
/// always dropped (`Recorder::stop`) *before* the pipeline's first `.await`.
pub struct VoiceSubsystem {
    /// The single GPU entry point (orchestration crate's implementor). STT jobs go
    /// here at priority 100; never constructed by this crate (doc 15 §4).
    scheduler: Arc<dyn GpuScheduler>,
    /// Events / ctx_vec / connector_state (doc 03) — the single-writer handle.
    db: Arc<Db>,
    /// 768-d embedder (nomic-embed, doc 03 §5) — same model as ingest.
    embedder: Arc<dyn Embedder>,
    config: VoiceConfig,
    /// Live only while capture is ON (doc 12 §6); dropping it unregisters the chord.
    hotkey: Option<hotkey::PttHotkey>,
    /// Live only between `ptt_down` and `ptt_up`.
    recorder: Option<audio_capture::Recorder>,
}

impl VoiceSubsystem {
    /// Construct the subsystem with its injected GPU scheduler, DB, embedder, and
    /// settings. Registering the hotkey and opening the mic happen on
    /// [`enable`](Self::enable) so the capture toggle flips voice on/off cheaply
    /// (doc 12 §6).
    pub fn new(
        scheduler: Arc<dyn GpuScheduler>,
        db: Arc<Db>,
        embedder: Arc<dyn Embedder>,
        config: VoiceConfig,
    ) -> Self {
        Self {
            scheduler,
            db,
            embedder,
            config,
            hotkey: None,
            recorder: None,
        }
    }

    /// Capture toggle ON: register the global PTT hotkey (doc 07 §2) and probe the
    /// default mic so a denied permission / missing device surfaces now as a
    /// one-time notice (doc 07 §2) rather than on the first press.
    ///
    /// **UNVERIFIED (hardware):** touches `global-hotkey` + `cpal`.
    pub fn enable(&mut self) -> Result<(), VoiceError> {
        let hotkey = hotkey::PttHotkey::register(self.config.chord.clone())
            .map_err(|e| VoiceError::HotkeyConflict(e.to_string()))?;
        // Probe: open then immediately drop a capture to detect a denied permission
        // or absent device before the user ever holds the key.
        audio_capture::Recorder::start()
            .map_err(|e| VoiceError::MicUnavailable(e.to_string()))?
            .stop()
            .map_err(|e| VoiceError::MicUnavailable(e.to_string()))?;
        self.hotkey = Some(hotkey);
        Ok(())
    }

    /// Capture toggle OFF: unregister the hotkey and drop any live mic stream so
    /// PTT goes inert in <3 s (doc 12 §6). Sidecar/VRAM teardown is the
    /// orchestrator's job, not ours.
    pub fn disable(&mut self) {
        self.recorder = None; // drop any in-flight capture (stops the WASAPI stream)
        self.hotkey = None; // unregister the chord (Drop)
    }

    /// Key-down: start WASAPI capture into a native buffer; the shell shows the
    /// "listening" pill (doc 07 §2, doc 11). The 30 s ceiling ([`MAX_UTTERANCE`])
    /// is enforced by the caller comparing [`audio_capture::Recorder::elapsed`].
    ///
    /// **UNVERIFIED (hardware):** opens a `cpal` input stream.
    pub fn ptt_down(&mut self) -> Result<(), VoiceError> {
        let recorder =
            audio_capture::Recorder::start().map_err(|e| VoiceError::MicUnavailable(e.to_string()))?;
        self.recorder = Some(recorder);
        Ok(())
    }

    /// Key-up: stop capture and run the full per-utterance pipeline (doc 07 §2-§5).
    ///
    /// **UNVERIFIED (hardware) only up to the PCM** — capture stop is real mic I/O;
    /// everything after is [`process_utterance`](Self::process_utterance), which is
    /// tested. The `!Send` stream is dropped by `stop()` before the first `.await`.
    pub async fn ptt_up(&mut self) -> Result<UtteranceOutcome, VoiceError> {
        let recorder = self
            .recorder
            .take()
            .ok_or_else(|| VoiceError::Stt("ptt_up without an active capture".into()))?;
        let pcm = recorder.stop().map_err(|e| VoiceError::MicUnavailable(e.to_string()))?;
        self.process_utterance(pcm, now_epoch_ms()).await
    }

    /// The per-utterance pipeline (doc 07 §2-§5), factored out of live capture so it
    /// is fully testable. **Order is load-bearing: store + embed happen before any
    /// intent branch**, so the telemetry role is truly unconditional (locked
    /// decision B, doc 07 §3).
    pub async fn process_utterance(
        &self,
        pcm: audio_capture::PcmBuffer,
        now_ms: i64,
    ) -> Result<UtteranceOutcome, VoiceError> {
        // 1. VAD trim + accidental-tap gate (doc 07 §2): a sub-300 ms tap stores
        //    nothing (no real utterance occurred).
        let trimmed = vad::trim(&pcm).map_err(|e| VoiceError::Stt(e.to_string()))?;
        if trimmed.is_accidental_tap() {
            return Ok(UtteranceOutcome::DiscardedTap);
        }

        // 2. STT GpuJob (priority 100, never cancellable) via the injected scheduler
        //    — CPU fallback is the orchestrator's call, not ours (doc 07 §3).
        let job = stt_job::build(trimmed.speech.to_wav());
        let output = self
            .scheduler
            .enqueue(job)
            .await
            .map_err(|e| VoiceError::Stt(e.to_string()))?;
        let transcription = stt_job::Transcription::from_job_output(output)
            .ok_or_else(|| VoiceError::Stt("scheduler returned a non-STT output".into()))?;

        // 3. Classify once — drives BOTH the logged intent and the branch.
        let classified =
            intent_classifier::classify(&transcription.transcript, transcription.confidence);

        // 4. Store + embed — ALWAYS, before any branch (locked decision B, doc 07 §3).
        let record = utterance_logger::UtteranceRecord {
            transcript: transcription.transcript.clone(),
            duration_ms: transcription.duration_ms,
            stt_model: self.config.stt_model.clone(),
            confidence: transcription.confidence,
            intent: utterance_logger::LoggedIntent::from_classified(classified.intent),
        };
        utterance_logger::store_and_embed(&self.db, self.embedder.as_ref(), &record, now_ms)
            .await
            .map_err(|e| VoiceError::Logging(e.to_string()))?;

        // 5. Low confidence ⇒ confirm chip; never act on a guess (doc 07 §4.4).
        if classified.needs_confirmation() {
            return Ok(UtteranceOutcome::ConfirmChip { transcript: transcription.transcript });
        }

        // 6. Branch on intent (doc 07 §4-§5).
        match classified.intent {
            intent_classifier::Intent::Escalation => {
                Ok(UtteranceOutcome::EscalationDraft { transcript: transcription.transcript })
            }
            intent_classifier::Intent::Query => {
                let bubble = retrieval::run(
                    &self.db,
                    self.embedder.as_ref(),
                    &transcription.transcript,
                    now_ms,
                )
                .await
                .map_err(|e| VoiceError::Retrieval(e.to_string()))?;
                Ok(UtteranceOutcome::Answer(bubble))
            }
            intent_classifier::Intent::Telemetry => Ok(UtteranceOutcome::StoredSilently),
        }
    }
}

/// Wall-clock epoch ms — the clock the store path stamps utterances with (doc 03),
/// consistent with the rest of the system's epoch-ms lifetimes.
fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_capture::{PcmBuffer, TARGET_SAMPLE_RATE};
    use aperture_contracts::fakes::FakeScheduler;
    use aperture_contracts::gpu_job::JobOutput;
    use aperture_embedding::HashEmbedder;

    /// A subsystem whose STT scheduler returns a canned transcript + confidence.
    fn subsystem(transcript: &str, confidence: f32) -> VoiceSubsystem {
        let scheduler = Arc::new(FakeScheduler {
            latency: Duration::ZERO,
            refuse_with_projection_gb: None,
            canned: Some(JobOutput::Stt {
                transcript: transcript.to_string(),
                avg_token_confidence: confidence,
                duration_ms: 800,
            }),
        });
        let db = Arc::new(Db::open_in_memory().expect("in-memory db"));
        VoiceSubsystem::new(scheduler, db, Arc::new(HashEmbedder), VoiceConfig::default())
    }

    /// `ms` of loud tone — passes the VAD speech gate (the FakeScheduler ignores
    /// the WAV, so the PCM only has to clear the tap threshold).
    fn speech_pcm(ms: u32) -> PcmBuffer {
        let n = (TARGET_SAMPLE_RATE * ms / 1000) as usize;
        PcmBuffer { samples: (0..n).map(|i| if i % 2 == 0 { 8000 } else { -8000 }).collect() }
    }

    fn event_count(db: &Db) -> i64 {
        db.with_conn(|c| c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)))
            .unwrap()
    }

    #[tokio::test]
    async fn accidental_tap_discards_and_stores_nothing() {
        let vs = subsystem("hello there", 0.9);
        let out = vs.process_utterance(speech_pcm(100), 1_000).await.unwrap();
        assert!(matches!(out, UtteranceOutcome::DiscardedTap));
        assert_eq!(event_count(&vs.db), 0, "a tap stores nothing (no real utterance)");
    }

    #[tokio::test]
    async fn query_returns_an_answer_and_stores_the_utterance() {
        let vs = subsystem("find the rust tutorial", 0.9);
        let out = vs.process_utterance(speech_pcm(600), 1_000).await.unwrap();
        assert!(matches!(out, UtteranceOutcome::Answer(_)));
        assert_eq!(event_count(&vs.db), 1, "the utterance is stored unconditionally");
    }

    #[tokio::test]
    async fn low_confidence_shows_a_confirm_chip_but_still_stores() {
        let vs = subsystem("find the doc", 0.5); // < 0.6 floor
        let out = vs.process_utterance(speech_pcm(600), 1_000).await.unwrap();
        assert!(matches!(out, UtteranceOutcome::ConfirmChip { .. }), "never act on a guess");
        assert_eq!(event_count(&vs.db), 1, "stored before the confirm gate (telemetry unconditional)");
    }

    #[tokio::test]
    async fn ask_claude_is_an_escalation_draft_never_auto_sent() {
        let vs = subsystem("ask claude to summarize this", 0.9);
        let out = vs.process_utterance(speech_pcm(600), 1_000).await.unwrap();
        assert!(matches!(out, UtteranceOutcome::EscalationDraft { .. }));
        assert_eq!(event_count(&vs.db), 1);
    }

    #[tokio::test]
    async fn plain_statement_is_stored_silently() {
        let vs = subsystem("note to self buy milk", 0.9);
        let out = vs.process_utterance(speech_pcm(600), 1_000).await.unwrap();
        assert!(matches!(out, UtteranceOutcome::StoredSilently));
        assert_eq!(event_count(&vs.db), 1);
    }

    #[tokio::test]
    async fn stt_failure_errors_and_stores_nothing() {
        // A refused STT job must surface as VoiceError::Stt AND store nothing:
        // store+embed runs AFTER the STT job (step 4 after step 2), so a failed
        // transcription persists no row — the unconditional-store invariant only
        // applies once there IS a transcription (locked decision B, doc 07 §3).
        let scheduler = Arc::new(FakeScheduler {
            latency: Duration::ZERO,
            refuse_with_projection_gb: Some(7.9),
            canned: None,
        });
        let db = Arc::new(Db::open_in_memory().expect("in-memory db"));
        let vs = VoiceSubsystem::new(scheduler, Arc::clone(&db), Arc::new(HashEmbedder), VoiceConfig::default());
        let out = vs.process_utterance(speech_pcm(600), 1_000).await;
        assert!(matches!(out, Err(VoiceError::Stt(_))), "failed STT → VoiceError::Stt; got {out:?}");
        assert_eq!(event_count(&db), 0, "a failed transcription stores nothing (store is post-STT)");
    }
}
