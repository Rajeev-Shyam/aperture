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
//!   3. quality-filter lines, 4. emit concatenated text + mean confidence,
//!      plus the surviving lines' word bounding boxes (`OcrOutput::lines`,
//!      doc 24 decision #3) for the image-redaction gate.
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

use crate::ocr_engine::{OcrEngine as OcrEngineTrait, OcrLine, OcrOutput, OcrWord};
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

/// Filter + aggregate raw OCR lines (text only, no geometry) into the
/// [`OcrOutput`] shape (doc 06 §2). Shared by the real engine and tests (pure).
///
/// Delegates to [`aggregate_ocr_lines`] with empty word lists, so `text` and
/// `mean_confidence` are exactly what the geometry-bearing path produces for
/// the same strings; `lines` carries the kept lines with no words.
pub fn aggregate_lines(lines: Vec<String>) -> OcrOutput {
    aggregate_ocr_lines(
        lines.into_iter().map(|text| OcrLine { text, words: Vec::new() }).collect(),
    )
}

/// Filter + aggregate recognized lines **with word geometry** into the
/// [`OcrOutput`] shape (doc 06 §2; doc 24 decision #3).
///
/// The quality filter ([`line_quality`] ≥ [`MIN_LINE_CONFIDENCE`]) is applied
/// per line on `line.text`; a dropped line drops its words with it, a kept line
/// keeps them. `text` is the kept lines' text joined by `\n` and
/// `mean_confidence` the mean quality over kept lines — byte-for-byte what
/// [`aggregate_lines`] has always produced (the text feeds embeddings and
/// pattern signatures, so this must not drift).
pub fn aggregate_ocr_lines(lines: Vec<OcrLine>) -> OcrOutput {
    let mut kept: Vec<OcrLine> = Vec::new();
    let mut quality_sum = 0.0f32;
    for line in lines {
        let q = line_quality(&line.text);
        if q >= MIN_LINE_CONFIDENCE {
            quality_sum += q;
            kept.push(line);
        }
    }
    if kept.is_empty() {
        return OcrOutput { text: String::new(), mean_confidence: 0.0, lines: Vec::new() };
    }
    let mean = quality_sum / kept.len() as f32;
    let text = kept.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    OcrOutput { text, mean_confidence: mean, lines: kept }
}

/// Convert a WinRT `Foundation::Rect` (f32 `X/Y/Width/Height`, in the pixel
/// space of the bitmap handed to `RecognizeAsync`) into integer box edges.
/// Rounds **outward** (floor the origin, ceil the far edge) so the box never
/// under-covers the glyphs it came from; negative/NaN/huge values saturate via
/// `as u32`. Pure, so it is unit-tested off-Windows too.
pub fn rect_to_box(x: f32, y: f32, width: f32, height: f32) -> (u32, u32, u32, u32) {
    let x0 = x.floor().max(0.0) as u32;
    let y0 = y.floor().max(0.0) as u32;
    let x1 = (x + width).ceil().max(0.0) as u32;
    let y1 = (y + height).ceil().max(0.0) as u32;
    (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
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
    ///
    /// Returns raw (pre-quality-filter) lines **with word boxes** (doc 24
    /// decision #3): each `OcrLine::Words()` item yields `Text()` +
    /// `BoundingRect()` in the bitmap's pixel space — the same space as
    /// `bgra`, which is what the image-redaction gate recomposes.
    #[cfg(windows)]
    fn recognize_bgra(&self, bgra: &[u8], width: u32, height: u32) -> Result<Vec<OcrLine>, VisionError> {
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
                if text.trim().is_empty() {
                    continue;
                }
                // Word geometry (doc 24 decision #3). A failed Words() call
                // degrades to a geometry-less line rather than losing the text.
                let mut words = Vec::new();
                if let Ok(ws) = line.Words() {
                    for word in ws {
                        let (Ok(wtext), Ok(rect)) = (word.Text(), word.BoundingRect()) else {
                            continue;
                        };
                        let (x, y, w, h) = rect_to_box(rect.X, rect.Y, rect.Width, rect.Height);
                        words.push(OcrWord { text: wtext.to_string(), x, y, w, h });
                    }
                }
                lines.push(OcrLine { text, words });
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
            Ok(aggregate_ocr_lines(lines))
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
        assert!(empty.lines.is_empty());
    }

    fn word(text: &str, x: u32) -> OcrWord {
        OcrWord { text: text.to_string(), x, y: 10, w: 8 * text.len() as u32, h: 12 }
    }

    #[test]
    fn aggregate_ocr_lines_keeps_text_and_confidence_identical_to_aggregate_lines() {
        // Same strings through both paths ⇒ byte-identical text + confidence
        // (text feeds embeddings/pattern signatures; doc 24 decision #3 is additive).
        let strings = vec![
            "Budget Q3 – summary.xlsx".to_string(),
            "◊●¦¤§◊●¦¤§◊●".to_string(),
            "Total: 4,200".to_string(),
            "".to_string(),
        ];
        let with_geometry: Vec<OcrLine> = strings
            .iter()
            .map(|s| OcrLine {
                text: s.clone(),
                words: s.split_whitespace().enumerate().map(|(i, w)| word(w, i as u32 * 40)).collect(),
            })
            .collect();
        let a = aggregate_lines(strings.clone());
        let b = aggregate_ocr_lines(with_geometry);
        assert_eq!(a.text, b.text);
        assert_eq!(a.mean_confidence, b.mean_confidence);
        assert_eq!(a.lines.len(), b.lines.len());
        for (la, lb) in a.lines.iter().zip(&b.lines) {
            assert_eq!(la.text, lb.text);
            assert!(la.words.is_empty(), "the string path carries no geometry");
        }
    }

    #[test]
    fn words_survive_with_kept_lines_and_are_dropped_with_dropped_lines() {
        let out = aggregate_ocr_lines(vec![
            OcrLine { text: "my key sk-abcdefghijklmnop1234".into(), words: vec![word("my", 0), word("key", 30), word("sk-abcdefghijklmnop1234", 70)] },
            OcrLine { text: "◊●¦¤§◊●¦¤§◊●".into(), words: vec![word("◊●¦¤§◊●¦¤§◊●", 0)] },
            OcrLine { text: "Total: 4,200".into(), words: vec![word("Total:", 0), word("4,200", 60)] },
        ]);
        assert_eq!(out.text, "my key sk-abcdefghijklmnop1234\nTotal: 4,200");
        assert_eq!(out.lines.len(), 2, "the noise line (and its words) is dropped");
        assert_eq!(out.lines[0].words.len(), 3);
        assert_eq!(out.lines[0].words[2].text, "sk-abcdefghijklmnop1234");
        assert_eq!(out.lines[0].words[2].x, 70);
        assert_eq!(out.lines[1].words.len(), 2);
        assert!(out.lines.iter().all(|l| !l.text.contains('◊')));
        // `lines` mirrors `text` line-for-line, in order.
        let rejoined = out.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(rejoined, out.text);
    }

    #[test]
    fn rect_to_box_rounds_outward_and_saturates() {
        // Fractional rect covers px 10.4..=(10.4+20.2=30.6) ⇒ floor 10, ceil 31 ⇒ w 21.
        assert_eq!(rect_to_box(10.4, 3.9, 20.2, 8.0), (10, 3, 21, 9));
        // Exact integers pass through unchanged.
        assert_eq!(rect_to_box(5.0, 6.0, 7.0, 8.0), (5, 6, 7, 8));
        // Negative origins clamp to 0 (the far edge is kept).
        assert_eq!(rect_to_box(-3.0, -1.0, 10.0, 4.0), (0, 0, 7, 3));
        // NaN/huge values do not panic: NaN collapses to an empty box, an
        // infinite extent saturates.
        assert_eq!(rect_to_box(f32::NAN, 0.0, 1.0, 1.0), (0, 0, 0, 1));
        assert_eq!(rect_to_box(0.0, 0.0, f32::INFINITY, 1.0), (0, 0, u32::MAX, 1));
    }

    #[test]
    fn ocr_output_lines_is_serde_default_for_older_rows() {
        // Rows serialized before doc 24 #3 carried no `lines`; they must still load.
        let legacy = r#"{"text":"hello","mean_confidence":0.9}"#;
        let out: OcrOutput = serde_json::from_str(legacy).expect("legacy row loads");
        assert_eq!(out.text, "hello");
        assert!(out.lines.is_empty());
    }
}
