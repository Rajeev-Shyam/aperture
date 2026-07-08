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
//! ## Verified vs on-hardware
//! [`PcmBuffer::to_wav`] and [`downmix_resample`] are **pure + unit-tested**. The
//! [`Recorder`] itself drives `cpal` (WASAPI) and is **UNVERIFIED** — it compiles
//! against cpal 0.15 but has not been exercised on a real mic (no device in CI).
//! [VERIFY on the RTX box]: cpal's default-input format on Win11, and that the
//! held-key drop-to-stop reliably reaps the WASAPI client.

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

    /// Wrap the raw PCM in a minimal canonical 44-byte RIFF/WAVE header + samples
    /// — the byte form the STT `GpuJob` carries (doc 07 §3; `GpuJobKind::Stt { wav }`,
    /// doc 15 §4). 16 kHz, mono, 16-bit signed little-endian PCM.
    pub fn to_wav(&self) -> Vec<u8> {
        let data_len = (self.samples.len() * 2) as u32;
        let byte_rate = TARGET_SAMPLE_RATE * TARGET_CHANNELS as u32 * 2;
        let block_align = TARGET_CHANNELS * 2;
        let mut out = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes()); // chunk size
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
        out.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
        out.extend_from_slice(&TARGET_CHANNELS.to_le_bytes());
        out.extend_from_slice(&TARGET_SAMPLE_RATE.to_le_bytes());
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in &self.samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }
}

/// Clamp + scale one `f32` sample in `[-1, 1]` to `i16` PCM.
fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Down-mix interleaved `channels`-channel native `f32` audio to mono and
/// linear-resample it from `src_rate` to [`TARGET_SAMPLE_RATE`], returning 16-bit
/// PCM (doc 07 §2). **Pure + tested** — the resampler cpal callers rely on.
///
/// Linear interpolation is adequate for speech at these rates; a higher-quality
/// (sinc) resampler is a possible on-hardware upgrade if STT WER suffers.
pub fn downmix_resample(native: &[f32], src_rate: u32, channels: u16) -> Vec<i16> {
    if native.is_empty() || channels == 0 || src_rate == 0 {
        return Vec::new();
    }
    let ch = channels as usize;
    // 1. Down-mix interleaved frames to mono by averaging the channels.
    let mono: Vec<f32> = native
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    // 2. Resample mono src_rate -> TARGET_SAMPLE_RATE.
    if src_rate == TARGET_SAMPLE_RATE {
        return mono.into_iter().map(f32_to_i16).collect();
    }
    let dst_len = (mono.len() as u64 * TARGET_SAMPLE_RATE as u64 / src_rate as u64) as usize;
    let mut out = Vec::with_capacity(dst_len);
    for i in 0..dst_len {
        let src_pos = i as f64 * src_rate as f64 / TARGET_SAMPLE_RATE as f64;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = mono.get(idx).copied().unwrap_or(0.0);
        let b = mono.get(idx + 1).copied().unwrap_or(a);
        out.push(f32_to_i16(a + (b - a) * frac));
    }
    out
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
///
/// **UNVERIFIED (on-hardware):** drives a `cpal` input stream. `cpal::Stream` is
/// `!Send`, so the [`crate::VoiceSubsystem`] must own the recorder on the single
/// PTT thread (never across an `.await`) — the facade runs the hotkey loop on a
/// dedicated OS thread for exactly this reason (doc 07 §2).
pub struct Recorder {
    stream: cpal::Stream,
    /// Accumulated native-rate, native-channel `f32` samples from the callback.
    buf: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
    src_rate: u32,
    channels: u16,
    started: std::time::Instant,
}

impl Recorder {
    /// Open the default mic in WASAPI shared mode and begin buffering. Returns
    /// [`CaptureError::Unavailable`] if there is no device / permission is denied.
    pub fn start() -> Result<Self, CaptureError> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| CaptureError::Unavailable("no default input device".into()))?;
        let supported = device
            .default_input_config()
            .map_err(|e| CaptureError::Unavailable(e.to_string()))?;
        let src_rate = supported.sample_rate().0;
        let channels = supported.channels();
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<f32>::new()));
        let err_fn = |e| tracing::error!(error = %e, "cpal input stream error");

        // Each sample format needs its own typed callback; convert to f32 in-band.
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let sink = std::sync::Arc::clone(&buf);
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        if let Ok(mut b) = sink.lock() {
                            b.extend_from_slice(data);
                        }
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let sink = std::sync::Arc::clone(&buf);
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        if let Ok(mut b) = sink.lock() {
                            b.extend(data.iter().map(|s| *s as f32 / i16::MAX as f32));
                        }
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let sink = std::sync::Arc::clone(&buf);
                device.build_input_stream(
                    &config,
                    move |data: &[u16], _: &cpal::InputCallbackInfo| {
                        if let Ok(mut b) = sink.lock() {
                            b.extend(data.iter().map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0));
                        }
                    },
                    err_fn,
                    None,
                )
            }
            other => {
                return Err(CaptureError::Unavailable(format!(
                    "unsupported input sample format {other:?}"
                )))
            }
        }
        .map_err(|e| CaptureError::Stream(e.to_string()))?;

        stream.play().map_err(|e| CaptureError::Stream(e.to_string()))?;
        Ok(Self {
            stream,
            buf,
            src_rate,
            channels,
            started: std::time::Instant::now(),
        })
    }

    /// How long capture has been running — the facade compares this to
    /// [`crate::MAX_UTTERANCE`] to enforce the 30 s ceiling (doc 07 §2).
    pub fn elapsed(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    /// Stop the stream and return the captured audio resampled to 16 kHz mono PCM
    /// (doc 07 §2). Called on key-up or at the [`crate::MAX_UTTERANCE`] ceiling.
    pub fn stop(self) -> Result<PcmBuffer, CaptureError> {
        // Dropping the stream stops WASAPI capture (RAII); take the accumulated
        // native samples and resample to the canonical 16 kHz mono PCM.
        drop(self.stream);
        let native = self
            .buf
            .lock()
            .map(|mut b| std::mem::take(&mut *b))
            .unwrap_or_default();
        Ok(PcmBuffer {
            samples: downmix_resample(&native, self.src_rate, self.channels),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_has_a_canonical_44_byte_header_and_all_samples() {
        let pcm = PcmBuffer { samples: vec![0, 1, -1, 32767, -32768] };
        let wav = pcm.to_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        // header (44) + 5 samples * 2 bytes.
        assert_eq!(wav.len(), 44 + 5 * 2);
        // data chunk length field == sample bytes.
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len, 10);
        // sample rate + channels round-trip.
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), TARGET_CHANNELS);
        assert_eq!(
            u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]),
            TARGET_SAMPLE_RATE
        );
    }

    #[test]
    fn downmix_averages_stereo_to_mono() {
        // One stereo frame [1.0, -1.0] averages to 0.0.
        let out = downmix_resample(&[1.0, -1.0], TARGET_SAMPLE_RATE, 2);
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn resample_halves_length_from_32k_to_16k() {
        let native: Vec<f32> = (0..3200).map(|_| 0.5).collect(); // mono @ 32 kHz
        let out = downmix_resample(&native, 32_000, 1);
        assert_eq!(out.len(), 1600, "32k -> 16k halves the sample count");
        assert!(out.iter().all(|&s| (s - f32_to_i16(0.5)).abs() <= 1));
    }

    #[test]
    fn passthrough_at_target_rate_only_downmixes() {
        let native: Vec<f32> = vec![0.25; 800];
        let out = downmix_resample(&native, TARGET_SAMPLE_RATE, 1);
        assert_eq!(out.len(), 800);
    }

    #[test]
    fn empty_or_degenerate_input_is_safe() {
        assert!(downmix_resample(&[], 16_000, 1).is_empty());
        assert!(downmix_resample(&[0.1, 0.2], 0, 1).is_empty());
        assert!(downmix_resample(&[0.1, 0.2], 16_000, 0).is_empty());
    }
}
