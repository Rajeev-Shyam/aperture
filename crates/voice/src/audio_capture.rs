//! Microphone capture (doc 07 §2).
//!
//! WASAPI **shared-mode**, default-input device, resampled to **16 kHz mono PCM**
//! — the format Whisper / Silero VAD expect. Capture runs *only while the PTT key
//! is held*; key-up (or the 30 s ceiling, doc 07 §2) stops the stream. Audio
//! buffers + VAD are negligible against the VRAM budget (doc 07 §1).
//!
//! Mic-permission denial (or no default device) surfaces
//! [`CaptureError::Unavailable`], which the subsystem turns into a one-time
//! explanatory notice and disables PTT (doc 07 §2, doc 07 §6).
//!
//! TODO(M6:) implement on `cpal` (WASAPI host). [VERIFY] cpal's default-input
//! sample rate/format on Win11 and pick the resampler (cpal gives native rate;
//! we down-mix to mono and resample to 16 kHz ourselves).

/// Target capture format for STT/VAD (doc 07 §2).
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Mono — Whisper input is single-channel.
pub const TARGET_CHANNELS: u16 = 1;

/// 16 kHz mono PCM, the canonical buffer passed to VAD then wrapped as a WAV for
/// the STT job (doc 07 §2-§3).
#[derive(Debug, Clone, Default)]
pub struct PcmBuffer {
    /// Interleaved (mono) 16-bit samples at [`TARGET_SAMPLE_RATE`].
    pub samples: Vec<i16>,
}

impl PcmBuffer {
    /// Duration of the captured audio in milliseconds.
    pub fn duration_ms(&self) -> u32 {
        ((self.samples.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64) as u32
    }

    /// Wrap the raw PCM in a minimal RIFF/WAV container — the byte form the STT
    /// `GpuJob` carries (doc 07 §3; `GpuJobKind::Stt { wav }`, doc 15 §4).
    pub fn to_wav(&self) -> Vec<u8> {
        // TODO(M6:) emit a 16 kHz mono 16-bit PCM WAV header + sample bytes.
        todo!("M6: serialize PCM to a 16 kHz mono WAV for the STT GpuJob")
    }
}

/// Errors opening or running the capture stream.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// No default input device, or OS mic permission denied (doc 07 §2).
    #[error("microphone unavailable: {0}")]
    Unavailable(String),
    /// The audio backend failed mid-stream.
    #[error("audio stream error: {0}")]
    Stream(String),
}

/// A live, held-key capture session. Created on `ptt_down`, consumed on `ptt_up`.
pub struct Recorder {
    // stream: cpal::Stream,                     // !Send on some hosts — keep on one thread
    // buf: Arc<Mutex<Vec<f32>>>,                // accumulated native-rate samples
    // started: std::time::Instant,              // for the 30 s ceiling (doc 07 §2)
}

impl Recorder {
    /// Open the default mic in WASAPI shared mode and begin buffering. Returns
    /// [`CaptureError::Unavailable`] if there is no device / permission is denied.
    pub fn start() -> Result<Self, CaptureError> {
        // TODO(M6:) cpal default_host().default_input_device() -> build_input_stream;
        //           detect a denied permission (device present but stream errors) and
        //           map to Unavailable.
        todo!("M6: open default mic (WASAPI shared mode), start buffering")
    }

    /// Stop the stream and return the captured audio resampled to 16 kHz mono PCM
    /// (doc 07 §2). Called on key-up or at the [`crate::MAX_UTTERANCE`] ceiling.
    pub fn stop(self) -> Result<PcmBuffer, CaptureError> {
        // TODO(M6:) drop the stream; down-mix to mono; resample native -> 16 kHz.
        todo!("M6: stop stream; down-mix + resample to 16 kHz mono PCM")
    }
}
