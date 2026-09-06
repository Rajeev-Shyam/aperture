//! Frame → redacted, downscaled JPEG (Doc 22 §3.2 "screenshot handling",
//! owner decision #3 "build image redaction before it ships wider").
//!
//! Order is the privacy argument, so it is fixed here and not configurable:
//!
//! 1. **OCR with word geometry** on the frame at OCR scale (≤ 1600 px, the
//!    same `downscale_bgra` Layer A uses) — [`OcrOutput::lines`] carries every
//!    surviving word's bounding box.
//! 2. **Image redaction at that same scale** — `aperture_privacy::image_redaction`
//!    runs the text rules over each OCR line and paints an opaque block over
//!    every word a rule covered. Same coordinate space as step 1, so there is
//!    no box re-mapping to get wrong.
//! 3. **Downscale to the payload edge** (768 px, Doc 22 §3.2) and JPEG-encode.
//!    Shrinking *after* painting means a redaction box can only ever get
//!    blurrier, never smaller than the text it covers.
//!
//! The OCR text handed back is already redacted with the same rules, so the
//! caller can stage it as `ocr_text` directly (`build_step_payload` redacts
//! it again — idempotent on text, and a second line of defence).
//!
//! Raw pixels never leave this function except as the encoded, redacted JPEG.

use aperture_privacy::image_redaction::{redact_bgra, ImageRedactionReport, LineBoxes, WordBox};
use aperture_privacy::redaction::Redactor;
use aperture_vision_ocr::frame_processor::downscale_bgra;
use aperture_vision_ocr::windows_media_ocr::OCR_MAX_LONG_EDGE_PX;
use aperture_vision_ocr::{OcrEngine, OcrOutput};

/// Doc 22 §3.2: payload screenshots are downscaled to a 768 px long edge
/// (ADR-032's VLM adaptive path uses 1024; the payload goes further because it
/// leaves the machine on every step).
pub const SCREENSHOT_MAX_LONG_EDGE_PX: u32 = 768;
/// JPEG quality for the payload screenshot — same q85 as the VLM path.
pub const SCREENSHOT_JPEG_QUALITY: u8 = 85;

#[derive(Debug, thiserror::Error)]
pub enum ScreenshotError {
    #[error("ocr: {0}")]
    Ocr(String),
    #[error("image: {0}")]
    Image(String),
    /// A text rule hit a line whose word geometry is missing or incomplete,
    /// so the pixels it covers could not be painted with certainty — the
    /// frame is withheld and the step goes text-only (fail closed, 08-22
    /// review).
    #[error("redaction: {0}")]
    Redaction(String),
}

/// A redacted payload screenshot, ready to base64 (Doc 22 §3.2).
#[derive(Debug, Clone)]
pub struct RedactedScreenshot {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// What one observation of the screen yields, post-redaction.
#[derive(Debug, Clone)]
pub struct ObservedScreen {
    /// Redacted OCR text (line-joined, same shape Layer A stores).
    pub ocr_text: String,
    pub ocr: OcrOutput,
    pub screenshot: RedactedScreenshot,
    pub image_redaction: ImageRedactionReport,
}

/// Run steps 1–3 above over one raw BGRA8 frame.
pub fn observe_frame(
    bgra: &[u8],
    width: u32,
    height: u32,
    engine: &dyn OcrEngine,
    redactor: &Redactor,
) -> Result<ObservedScreen, ScreenshotError> {
    if width == 0 || height == 0 || bgra.len() < (width as usize) * (height as usize) * 4 {
        return Err(ScreenshotError::Image("empty or truncated frame".into()));
    }
    // 1. OCR at Layer A's scale, with word boxes.
    let (mut ocr_scale, ow, oh) = downscale_bgra(bgra, width, height, OCR_MAX_LONG_EDGE_PX)
        .map_err(|e| ScreenshotError::Image(e.to_string()))?;
    let ocr = engine
        .process_frame(&ocr_scale, ow, oh)
        .map_err(|e| ScreenshotError::Ocr(e.to_string()))?;

    // 2. Paint over every word a text rule covers — same coordinate space.
    //    The quality-DROPPED lines go through the gate too (08-22 review):
    //    their text never reaches `ocr.text`, but their pixels are still in
    //    the frame. Each line carries its own text so the gate can tell
    //    whether its geometry is complete.
    let lines: Vec<LineBoxes> = ocr
        .lines
        .iter()
        .chain(ocr.dropped_lines.iter())
        .map(|l| LineBoxes {
            text: l.text.clone(),
            words: l
                .words
                .iter()
                .map(|w| WordBox { text: w.text.clone(), x: w.x, y: w.y, w: w.w, h: w.h })
                .collect(),
        })
        .collect();
    let image_redaction = redact_bgra(&mut ocr_scale, ow, oh, &lines, redactor);
    if image_redaction.lines_unpaintable > 0 {
        // Fail closed: a rule hit text with no (complete) box under it — the
        // text path redacts it, the pixels might still show it. No frame.
        return Err(ScreenshotError::Redaction(format!(
            "{} line(s) a rule hit have missing or incomplete word geometry — frame withheld",
            image_redaction.lines_unpaintable
        )));
    }
    let (ocr_text, _) = redactor.redact_text(&ocr.text);

    // 3. Shrink to the payload edge and encode.
    let (small, sw, sh) = downscale_bgra(&ocr_scale, ow, oh, SCREENSHOT_MAX_LONG_EDGE_PX)
        .map_err(|e| ScreenshotError::Image(e.to_string()))?;
    let jpeg = encode_jpeg_bgra(&small, sw, sh, SCREENSHOT_JPEG_QUALITY)?;

    Ok(ObservedScreen {
        ocr_text,
        ocr,
        screenshot: RedactedScreenshot { jpeg, width: sw, height: sh },
        image_redaction,
    })
}

/// BGRA8 → JPEG (RGB, no alpha). Mirrors `vlm_layer::prepare_image` minus the
/// resize, which happens in `observe_frame` at the payload edge.
pub fn encode_jpeg_bgra(
    bgra: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>, ScreenshotError> {
    let mut rgb = Vec::with_capacity((width as usize) * (height as usize) * 3);
    for px in bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    let buf = image::RgbImage::from_raw(width, height, rgb)
        .ok_or_else(|| ScreenshotError::Image("buffer size mismatch".into()))?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&image::DynamicImage::ImageRgb8(buf))
        .map_err(|e| ScreenshotError::Image(e.to_string()))?;
    Ok(out.into_inner())
}

/// Standard base64 (no line breaks) — the `screenshot_b64` wire form.
pub fn to_base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_vision_ocr::ocr_engine::{OcrLine, OcrWord};
    use aperture_vision_ocr::VisionError;

    /// A fake engine that "reads" a fixed set of words with boxes (`.0`), plus
    /// the lines a quality filter would have dropped, geometry intact (`.1`).
    struct FixedOcr(Vec<OcrLine>, Vec<OcrLine>);
    impl OcrEngine for FixedOcr {
        fn process_frame(&self, _: &[u8], _: u32, _: u32) -> Result<OcrOutput, VisionError> {
            let text = self.0.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join("\n");
            Ok(OcrOutput { text, mean_confidence: 0.9, lines: self.0.clone(), dropped_lines: self.1.clone() })
        }
        fn engine_id(&self) -> &'static str {
            "fixed"
        }
    }

    fn word(text: &str, x: u32) -> OcrWord {
        OcrWord { text: text.into(), x, y: 4, w: 30, h: 10 }
    }

    fn white_frame(w: u32, h: u32) -> Vec<u8> {
        vec![255u8; (w * h * 4) as usize]
    }

    #[test]
    fn secret_words_are_blocked_before_the_jpeg_exists_and_text_is_redacted() {
        let secret = "sk-abcdefghijklmnop1234";
        let engine = FixedOcr(
            vec![OcrLine {
                text: format!("token {secret} ok"),
                words: vec![word("token", 0), word(secret, 40), word("ok", 80)],
            }],
            vec![],
        );
        let redactor = Redactor::new(&[]).unwrap();
        let out = observe_frame(&white_frame(128, 32), 128, 32, &engine, &redactor).unwrap();
        assert_eq!(out.image_redaction.boxes_painted, 1, "exactly the secret's box");
        assert!(!out.ocr_text.contains(secret), "ocr text redacted: {}", out.ocr_text);
        assert!(out.ocr_text.contains("token"), "clean words survive");
        assert!(out.screenshot.jpeg.starts_with(&[0xFF, 0xD8]), "JPEG SOI marker");
        assert_eq!((out.screenshot.width, out.screenshot.height), (128, 32));
    }

    /// 08-22 review [low]: the engine gave a line's text but no word boxes
    /// (`Words()` failed) and a rule hits it — nothing can be painted with
    /// certainty, so the frame is withheld and the caller degrades to a
    /// text-only observation.
    #[test]
    fn a_hit_line_without_geometry_withholds_the_frame() {
        let engine = FixedOcr(
            vec![OcrLine { text: "token sk-abcdefghijklmnop1234 ok".into(), words: vec![] }],
            vec![],
        );
        let redactor = Redactor::new(&[]).unwrap();
        let err = observe_frame(&white_frame(128, 32), 128, 32, &engine, &redactor).unwrap_err();
        assert!(matches!(err, ScreenshotError::Redaction(_)), "{err}");
    }

    /// …but a geometry-less line NO rule hits costs nothing: the frame ships.
    #[test]
    fn a_clean_line_without_geometry_still_ships() {
        let engine = FixedOcr(vec![OcrLine { text: "hello world".into(), words: vec![] }], vec![]);
        let redactor = Redactor::new(&[]).unwrap();
        let out = observe_frame(&white_frame(128, 32), 128, 32, &engine, &redactor).unwrap();
        assert_eq!(out.image_redaction.lines_unpaintable, 0);
        assert_eq!(out.ocr_text, "hello world");
    }

    /// 08-22 review [low]: a quality-DROPPED line never reaches the text, but
    /// its pixels are in the frame — the gate paints it all the same, and the
    /// black block survives into the JPEG.
    #[test]
    fn quality_dropped_lines_are_painted_but_never_in_the_text() {
        let secret = "sk-abcdefghijklmnop1234";
        let engine = FixedOcr(
            vec![OcrLine { text: "hello".into(), words: vec![word("hello", 0)] }],
            vec![OcrLine { text: format!("¦¦ {secret}"), words: vec![word("¦¦", 0), word(secret, 40)] }],
        );
        let redactor = Redactor::new(&[]).unwrap();
        let out = observe_frame(&white_frame(128, 32), 128, 32, &engine, &redactor).unwrap();
        assert_eq!(out.image_redaction.boxes_painted, 1, "the dropped line's secret box");
        assert_eq!(out.image_redaction.lines_unpaintable, 0);
        assert_eq!(out.ocr_text, "hello", "dropped text stays out of the text path");
        // No downscale at 128×32, so the box (x 40..70, y 4..14) maps 1:1.
        let img = image::load_from_memory(&out.screenshot.jpeg).unwrap().to_rgb8();
        let inside = img.get_pixel(55, 9);
        assert!(inside[0] < 48 && inside[1] < 48 && inside[2] < 48, "painted black in the JPEG: {inside:?}");
        let outside = img.get_pixel(10, 24);
        assert!(outside[0] > 200 && outside[1] > 200 && outside[2] > 200, "untouched pixel stays white: {outside:?}");
    }

    #[test]
    fn payload_edge_is_768_and_never_upscales() {
        let engine = FixedOcr(vec![], vec![]);
        let redactor = Redactor::new(&[]).unwrap();
        let out = observe_frame(&white_frame(1920, 1080), 1920, 1080, &engine, &redactor).unwrap();
        assert_eq!(out.screenshot.width, 768);
        assert_eq!(out.screenshot.height, 432);
        let small = observe_frame(&white_frame(300, 200), 300, 200, &engine, &redactor).unwrap();
        assert_eq!((small.screenshot.width, small.screenshot.height), (300, 200));
    }

    /// Q-V2-02 (Doc 22 §12): does a 768 px screenshot fit the MCP result cap?
    /// A worst-case noisy 768×432 frame at q85 is measured here against the
    /// 1 MiB cap (`MCP_RESULT_MAX_BYTES`, decision #42) with headroom for the
    /// JSON around it. Noise is the adversarial case — real screens compress
    /// far better.
    #[test]
    fn q_v2_02_noisy_768px_screenshot_fits_the_mcp_cap_with_headroom() {
        let (w, h) = (768u32, 432u32);
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        let mut seed = 0x9E37_79B9u32;
        for _ in 0..(w * h) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            bgra.extend_from_slice(&[seed as u8, (seed >> 8) as u8, (seed >> 16) as u8, 255]);
        }
        let jpeg = encode_jpeg_bgra(&bgra, w, h, SCREENSHOT_JPEG_QUALITY).unwrap();
        let b64_len = to_base64(&jpeg).len();
        eprintln!("Q-V2-02: noisy 768x432 q85 JPEG = {} B, base64 = {b64_len} B", jpeg.len());
        // 1 MiB cap minus 64 KiB for the rest of the payload.
        assert!(
            b64_len < (1024 * 1024) - (64 * 1024),
            "noisy 768px JPEG base64 is {b64_len} bytes — over the MCP headroom"
        );
    }
}
