//! Voice-activity detection / silence trim (doc 07 §2).
//!
//! Silero VAD trims leading and trailing silence from the captured buffer so the
//! STT job sees speech, not dead air. If the trimmed buffer contains **< 300 ms**
//! of speech, the gesture is treated as an accidental tap and discarded — nothing
//! is transcribed, nothing is stored (doc 07 §2). VAD is CPU-only and negligible
//! against the VRAM budget (doc 07 §1).
//!
//! TODO(M6:) run Silero VAD (ONNX) over 16 kHz mono frames. [VERIFY] the runtime
//! (e.g. `ort`/onnxruntime) and the model asset path; tune the speech-probability
//! threshold and frame size on real mic input.

use crate::audio_capture::PcmBuffer;

/// Minimum speech below which a gesture is an accidental tap and is discarded
/// (doc 07 §2).
pub const MIN_SPEECH_MS: u32 = 300;

/// Result of trimming: the speech-only buffer plus how much speech it contains.
#[derive(Debug)]
pub struct Trimmed {
    /// Leading/trailing silence removed; ready to wrap as a WAV (doc 07 §3).
    pub speech: PcmBuffer,
    /// Detected speech duration in ms; `< `[`MIN_SPEECH_MS`]` ⇒ discard.
    pub speech_ms: u32,
}

impl Trimmed {
    /// `true` when the gesture was an accidental tap and should be discarded
    /// (doc 07 §2). The subsystem maps this to [`crate::UtteranceOutcome::DiscardedTap`].
    pub fn is_accidental_tap(&self) -> bool {
        self.speech_ms < MIN_SPEECH_MS
    }
}

/// Errors loading or running the VAD model.
#[derive(Debug, thiserror::Error)]
pub enum VadError {
    /// The Silero model asset could not be loaded.
    #[error("VAD model unavailable: {0}")]
    ModelUnavailable(String),
    /// Inference failed.
    #[error("VAD inference failed: {0}")]
    Inference(String),
}

/// Trim leading/trailing silence with Silero VAD (doc 07 §2). The caller checks
/// [`Trimmed::is_accidental_tap`] before building the STT job.
pub fn trim(_pcm: &PcmBuffer) -> Result<Trimmed, VadError> {
    // TODO(M6:) frame the 16 kHz mono PCM; run Silero per-frame speech probability;
    //           find first/last speech frame; slice; sum speech_ms.
    todo!("M6: Silero VAD trim; compute speech_ms for the <300 ms tap gate")
}
