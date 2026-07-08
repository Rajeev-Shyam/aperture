//! Voice-activity detection / silence trim (doc 07 §2).
//!
//! Trims leading and trailing silence from the captured buffer so the STT job
//! sees speech, not dead air. If the trimmed buffer contains **< 300 ms** of
//! speech, the gesture is treated as an accidental tap and discarded — nothing is
//! transcribed, nothing is stored (doc 07 §2). VAD is CPU-only and negligible
//! against the VRAM budget (doc 07 §1).
//!
//! ## Backend
//! M6 ships a **deterministic energy-gate** (short-frame RMS threshold): pure,
//! dependency-free, and unit-tested, so the load-bearing behavior — the < 300 ms
//! accidental-tap gate — is verified now. The doc's **Silero VAD (ONNX)** is the
//! quality upgrade at the hardware gate: it slots in behind the same per-frame
//! speech-decision seam ([`frame_is_speech`]), replacing the RMS test with the
//! model's speech probability. [VERIFY on real mic]: the RMS threshold + frame
//! size, and whether Silero is worth its model asset over the energy gate.

use crate::audio_capture::{PcmBuffer, TARGET_SAMPLE_RATE};

/// Minimum speech below which a gesture is an accidental tap and is discarded
/// (doc 07 §2).
pub const MIN_SPEECH_MS: u32 = 300;

/// Analysis frame length in ms (doc 07 §2). 30 ms ≈ 480 samples at 16 kHz — the
/// standard VAD frame that Silero also uses, so the seam swaps cleanly.
pub const FRAME_MS: u32 = 30;

/// RMS (in `[0,1]`) at/above which a frame counts as speech. ≈ −38 dBFS.
/// [ASSUMPTION — tune on real mic input at the hardware gate, doc 07 §2].
pub const SPEECH_RMS_THRESHOLD: f32 = 0.012;

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
    /// The Silero model asset could not be loaded (Silero backend only).
    #[error("VAD model unavailable: {0}")]
    ModelUnavailable(String),
    /// Inference failed.
    #[error("VAD inference failed: {0}")]
    Inference(String),
}

/// The per-frame speech decision — the seam a Silero ONNX backend replaces. The
/// energy gate: a frame is speech when its RMS clears [`SPEECH_RMS_THRESHOLD`].
fn frame_is_speech(frame: &[i16]) -> bool {
    if frame.is_empty() {
        return false;
    }
    let sum_sq: f64 = frame
        .iter()
        .map(|s| {
            let f = *s as f64 / i16::MAX as f64;
            f * f
        })
        .sum();
    let rms = (sum_sq / frame.len() as f64).sqrt();
    rms as f32 >= SPEECH_RMS_THRESHOLD
}

/// Trim leading/trailing silence (doc 07 §2): find the first and last speech
/// frame, slice between them, and report the trimmed speech duration. The caller
/// checks [`Trimmed::is_accidental_tap`] before building the STT job. All-silence
/// input trims to empty with `speech_ms = 0`.
pub fn trim(pcm: &PcmBuffer) -> Result<Trimmed, VadError> {
    let frame_len = (TARGET_SAMPLE_RATE as usize * FRAME_MS as usize) / 1000;
    if frame_len == 0 || pcm.samples.is_empty() {
        return Ok(Trimmed { speech: PcmBuffer::default(), speech_ms: 0 });
    }
    let frames: Vec<&[i16]> = pcm.samples.chunks(frame_len).collect();
    let first = frames.iter().position(|f| frame_is_speech(f));
    let last = frames.iter().rposition(|f| frame_is_speech(f));
    match (first, last) {
        (Some(a), Some(b)) => {
            let start = a * frame_len;
            let end = ((b + 1) * frame_len).min(pcm.samples.len());
            let speech = pcm.samples[start..end].to_vec();
            let speech_ms = ((speech.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64) as u32;
            Ok(Trimmed { speech: PcmBuffer { samples: speech }, speech_ms })
        }
        // No frame cleared the threshold — pure silence.
        _ => Ok(Trimmed { speech: PcmBuffer::default(), speech_ms: 0 }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// N ms of a loud tone (|amplitude| well over the RMS threshold).
    fn tone(ms: u32) -> Vec<i16> {
        let n = (TARGET_SAMPLE_RATE * ms / 1000) as usize;
        (0..n).map(|i| if i % 2 == 0 { 8000 } else { -8000 }).collect()
    }
    fn silence(ms: u32) -> Vec<i16> {
        vec![0i16; (TARGET_SAMPLE_RATE * ms / 1000) as usize]
    }

    #[test]
    fn all_silence_is_an_accidental_tap() {
        let t = trim(&PcmBuffer { samples: silence(500) }).unwrap();
        assert_eq!(t.speech_ms, 0);
        assert!(t.is_accidental_tap());
        assert!(t.speech.samples.is_empty());
    }

    #[test]
    fn empty_buffer_is_a_tap() {
        let t = trim(&PcmBuffer::default()).unwrap();
        assert!(t.is_accidental_tap());
    }

    #[test]
    fn a_brief_blip_under_300ms_is_a_tap() {
        let mut samples = silence(100);
        samples.extend(tone(120)); // 120 ms of speech < 300 ms floor
        samples.extend(silence(100));
        let t = trim(&PcmBuffer { samples }).unwrap();
        assert!(t.speech_ms < MIN_SPEECH_MS, "got {} ms", t.speech_ms);
        assert!(t.is_accidental_tap());
    }

    #[test]
    fn real_speech_is_kept_and_silence_trimmed() {
        let mut samples = silence(200);
        samples.extend(tone(600)); // 600 ms of speech
        samples.extend(silence(200));
        let t = trim(&PcmBuffer { samples }).unwrap();
        assert!(!t.is_accidental_tap());
        assert!(t.speech_ms >= 500, "leading/trailing silence trimmed, got {} ms", t.speech_ms);
        // Trimmed buffer is far shorter than the 1000 ms captured.
        assert!(t.speech.duration_ms() < 800);
    }
}
