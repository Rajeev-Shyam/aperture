//! Default Layer-A engine: in-box `Windows.Media.Ocr` (doc 06 §2).
//!
//! Why in-box: fully local, per-language packs, fast on CPU, zero VRAM — exactly
//! the Tier-0 profile (doc 06 §1). [VERIFY accuracy on dense UI text; if
//! insufficient, swap RapidOCR/Tesseract behind the [`OcrEngine`] trait without
//! touching callers.]
//!
//! Pipeline for one frame (doc 06 §2):
//!   1. downscale to ≤ 1600 px long edge (done upstream in `FrameProcessor`),
//!   2. run `OcrEngine.RecognizeAsync` on a BGRA8 `SoftwareBitmap`,
//!   3. quality-filter lines, 4. emit concatenated text + mean confidence.
//!
//! **[VERIFY resolved — M2 implementation]: `Windows.Media.Ocr` surfaces NO
//! confidence values** (neither per-word nor per-line — `OcrWord` carries only
//! text + bounding rect). Doc 06 §2's "drop lines < 0.5 confidence" therefore
//! cannot be implemented literally with the in-box engine. Resolution:
//! - lines are filtered by a **text-quality heuristic** ([`line_quality`]):
//!   the fraction of sensible characters (alphanumeric / space / common
//!   punctuation) — gibberish-looking lines score low and are dropped at the
//!   same 0.5 floor;
//! - `mean_confidence` is the mean line quality over surviving lines — a
//!   *proxy*, honest-labelled here, feeding the doc 06 §4 weak-OCR wake branch;
//! - a confidence-bearing fallback engine (RapidOCR/Tesseract) restores true
//!   confidences behind the same trait if the proxy proves too coarse. Flagged
//!   for the M2 gate report.

use crate::ocr_engine::{OcrEngine as OcrEngineTrait, OcrOutput};
use crate::VisionError;

/// Max long-edge in pixels for the Layer-A downscale (doc 06 §2)
/// [ASSUMPTION: OCR quality/speed balance].
pub const OCR_MAX_LONG_EDGE_PX: u32 = 1600;

/// Lines whose quality heuristic is below this are dropped before the text
/// is concatenated (doc 06 §2; see the module note on confidence) [ASSUMPTION].
pub const MIN_LINE_CONFIDENCE: f32 = 0.5;

/// Fraction of "sensible" characters in a line — the confidence proxy (module
/// note): alphanumeric, whitespace, and common punctuation count as sensible.
pub fn line_quality(line: &str) -> f32 {
    let total = line.chars().count();
    if total == 0 {
        return 0.0;
    }
    let sensible = line
        .chars()
        .filter(|c| {
            c.is_alphanumeric()
                || c.is_whitespace()
                || matches!(c, '.' | ',' | ':' | ';' | '-' | '_' | '/' | '\\' | '(' | ')' | '\''
                    | '"' | '?' | '!' | '@' | '#' | '%' | '&' | '+' | '=' | '<' | '>' | '[' | ']'
                    | '{' | '}' | '|' | '*' | '~' | '$' | '€' | '£')
        })
        .count();
    sensible as f32 / total as f32
}

/// Filter + aggregate raw OCR lines into the [`OcrOutput`] shape (doc 06 §2).
/// Shared by the real engine and tests (pure).
pub fn aggregate_lines(lines: Vec<String>) -> OcrOutput {
    let mut kept = Vec::new();
    let mut quality_sum = 0.0f32;
    for line in lines {
        let q = line_quality(&line);
        if q >= MIN_LINE_CONFIDENCE {
            quality_sum += q;
            kept.push(line);
        }
    }
    if kept.is_empty() {
        return OcrOutput { text: String::new(), mean_confidence: 0.0 };
    }
    let mean = quality_sum / kept.len() as f32;
    OcrOutput { text: kept.join("\n"), mean_confidence: mean }
}

/// The in-box `Windows.Media.Ocr` engine.
///
/// Wraps a per-language `windows::Media::Ocr::OcrEngine`; constructed once per
/// language and reused (engine creation is comparatively expensive).
pub struct WindowsMediaOcr {
    #[cfg(windows)]
    engine: windows::Media::Ocr::OcrEngine,
    /// Whether the requested language fell back to a profile/en engine
    /// (doc 06 §6: "language pack missing ⇒ fall back + notice").
    pub language_fallback: bool,
}

// SAFETY: the WinRT OcrEngine is an agile object (WinRT class, marshals
// free-threaded); RecognizeAsync is stateless per call. Serialized use is
// guaranteed by the owning FrameProcessor. [VERIFY at the M2 gate.]
#[cfg(windows)]
unsafe impl Send for WindowsMediaOcr {}
#[cfg(windows)]
unsafe impl Sync for WindowsMediaOcr {}

impl WindowsMediaOcr {
    /// Construct an engine for `lang` (BCP-47), falling back to the user's
    /// profile languages, then `en` (doc 06 §6). [VERIFY language coverage.]
    #[cfg(windows)]
    pub fn new(lang: &str) -> Result<Self, VisionError> {
        use windows::core::HSTRING;
        use windows::Globalization::Language;
        use windows::Media::Ocr::OcrEngine;

        let mut language_fallback = false;
        let engine = Language::CreateLanguage(&HSTRING::from(lang))
            .ok()
            .and_then(|l| OcrEngine::TryCreateFromLanguage(&l).ok())
            .or_else(|| {
                language_fallback = true;
                OcrEngine::TryCreateFromUserProfileLanguages().ok()
            })
            .or_else(|| {
                Language::CreateLanguage(&HSTRING::from("en"))
                    .ok()
                    .and_then(|l| OcrEngine::TryCreateFromLanguage(&l).ok())
            })
            .ok_or_else(|| {
                VisionError::Ocr(format!("no OCR language pack usable (asked: {lang})"))
            })?;
        if language_fallback {
            tracing::warn!(lang, "OCR language pack missing; using profile/en fallback (doc 06 §6)");
        }
        Ok(Self { engine, language_fallback })
    }

    #[cfg(not(windows))]
    pub fn new(_lang: &str) -> Result<Self, VisionError> {
        Err(VisionError::Ocr("Windows.Media.Ocr is windows-only".into()))
    }

    /// Recognize one pre-downscaled BGRA8 buffer (doc 06 §2 steps 3-5).
    #[cfg(windows)]
    fn recognize_bgra(&self, bgra: &[u8], width: u32, height: u32) -> Result<Vec<String>, VisionError> {
        use windows::core::Interface;
        use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
        use windows::Storage::Streams::Buffer;
        use windows::Win32::System::WinRT::IBufferByteAccess;

        if bgra.len() != (width * height * 4) as usize {
            return Err(VisionError::Image("bgra buffer size mismatch".into()));
        }
        // windows crate: copy bytes into an IBuffer, then wrap as SoftwareBitmap.
        let buffer = Buffer::Create(bgra.len() as u32)
            .map_err(|e| VisionError::Image(e.to_string()))?;
        buffer
            .SetLength(bgra.len() as u32)
            .map_err(|e| VisionError::Image(e.to_string()))?;
        unsafe {
            let bytes: IBufferByteAccess =
                buffer.cast().map_err(|e| VisionError::Image(e.to_string()))?;
            let ptr = bytes.Buffer().map_err(|e| VisionError::Image(e.to_string()))?;
            std::ptr::copy_nonoverlapping(bgra.as_ptr(), ptr, bgra.len());
        }
        let bitmap = SoftwareBitmap::CreateCopyFromBuffer(
            &buffer,
            BitmapPixelFormat::Bgra8,
            width as i32,
            height as i32,
        )
        .map_err(|e| VisionError::Image(e.to_string()))?;

        let result = self
            .engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| VisionError::Ocr(e.to_string()))?
            .get()
            .map_err(|e| VisionError::Ocr(e.to_string()))?;

        let mut lines = Vec::new();
        for line in result.Lines().map_err(|e| VisionError::Ocr(e.to_string()))? {
            if let Ok(text) = line.Text() {
                let text = text.to_string();
                if !text.trim().is_empty() {
                    lines.push(text);
                }
            }
        }
        Ok(lines)
    }
}

impl OcrEngineTrait for WindowsMediaOcr {
    /// `frame` is a **raw BGRA8** buffer already downscaled to
    /// ≤ [`OCR_MAX_LONG_EDGE_PX`] by the caller ([`crate::FrameProcessor`]);
    /// `width`/`height` describe it. Budget ≤ 400 ms/frame (doc 06 §5) —
    /// measured at the M2 gate.
    fn process_frame(&self, frame: &[u8], width: u32, height: u32) -> Result<OcrOutput, VisionError> {
        #[cfg(windows)]
        {
            let lines = self.recognize_bgra(frame, width, height)?;
            Ok(aggregate_lines(lines))
        }
        #[cfg(not(windows))]
        {
            let _ = (frame, width, height);
            Err(VisionError::Ocr("windows-only".into()))
        }
    }

    fn engine_id(&self) -> &'static str {
        "windows-media-ocr"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_heuristic_separates_text_from_noise() {
        assert!(line_quality("Continue watching: Rust lifetimes (12:34)") > 0.9);
        assert!(line_quality("¦¦§¤◊◊●¦¤§") < 0.5, "glyph noise scores low");
        assert_eq!(line_quality(""), 0.0);
    }

    #[test]
    fn aggregate_drops_low_quality_lines_and_reports_proxy_confidence() {
        let out = aggregate_lines(vec![
            "Budget Q3 – summary.xlsx".to_string(),
            "◊●¦¤§◊●¦¤§◊●".to_string(),
            "Total: 4,200".to_string(),
        ]);
        assert!(out.text.contains("Budget"));
        assert!(out.text.contains("Total"));
        assert!(!out.text.contains('◊'), "noise line dropped (doc 06 §2)");
        assert!(out.mean_confidence >= MIN_LINE_CONFIDENCE);

        let empty = aggregate_lines(vec!["●●●●".to_string()]);
        assert_eq!(empty.text, "");
        assert_eq!(empty.mean_confidence, 0.0);
    }
}
