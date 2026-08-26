//! The image-redaction gate for screenshot payloads (doc 24 decision #3).
//!
//! Text payloads get the ordered 6-rule pass in [`crate::redaction`];
//! screenshots got nothing, so `PayloadItem::Screenshot` stays disabled
//! ("(v2)") until this gate exists. The approach is **OCR → redact →
//! recompose, reusing the text rules verbatim**: the caller OCRs the frame
//! (word boxes come from `aperture_vision_ocr::OcrOutput::lines`), converts
//! them to [`LineBoxes`], and [`redact_bgra`] paints over every word that the
//! text redactor would have replaced. One rule set, one truth: a secret the
//! preview would scrub from OCR text is scrubbed from the pixels too.
//!
//! ## Block, not blur
//! Redacted boxes are painted as an **opaque black rectangle**. Blur (and
//! pixelation) is partially invertible — the residual low-frequency signal can
//! be deconvolved or matched against candidate strings, which for a card
//! number or an API key is a real attack. A solid block carries no information
//! about what was under it; it is the honest choice for a privacy guarantee
//! (doc 13 §2 invariants).
//!
//! ## Dependencies
//! This crate must **not** depend on `aperture-vision-ocr` (privacy sits below
//! the pipeline crates); the serializer converts `OcrWord` → [`WordBox`]. The
//! painting is plain buffer arithmetic — no `image` crate.

use std::collections::HashMap;

use aperture_contracts::context_payload::Redaction;

use crate::redaction::{Redactor, RuleKind};

/// Pixels of padding painted around a redacted word box on every side, so
/// anti-aliased glyph edges and OCR box jitter do not leave a readable fringe
/// [ASSUMPTION: 2 px at the ≤ 1600 px OCR scale].
pub const BOX_PAD_PX: u32 = 2;

/// One OCR word with its bounding box, in the pixel space of the buffer handed
/// to [`redact_bgra`]. Mirrors `aperture_vision_ocr::OcrWord` without the
/// dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordBox {
    /// The word's text as OCR emitted it.
    pub text: String,
    /// Left edge, px.
    pub x: u32,
    /// Top edge, px.
    pub y: u32,
    /// Width, px.
    pub w: u32,
    /// Height, px.
    pub h: u32,
}

/// One OCR line's words in reading order. The line text the rules see is the
/// word texts joined with single spaces — so a card number OCR'd as four words
/// is still one Luhn-valid run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineBoxes {
    /// The words, in reading order.
    pub words: Vec<WordBox>,
}

/// What [`redact_bgra`] did, for the preview's `rule + count` line (doc 13 §5).
#[derive(Debug, Clone)]
pub struct ImageRedactionReport {
    /// Number of word boxes painted over.
    pub boxes_painted: usize,
    /// Per-rule hit counts (one hit per matched span, not per box), in
    /// [`RuleKind::ORDERED`] order — the same shape `redact_text` returns.
    pub redactions: Vec<Redaction>,
}

/// Paint opaque black over every OCR word the text rules would redact
/// (doc 24 decision #3).
///
/// `bgra` is a BGRA8 buffer of `width × height` px (the frame the OCR ran on,
/// so `lines` are in its pixel space). Per line the word texts are joined with
/// single spaces, [`Redactor::find_spans`] locates the hits, and any word whose
/// byte range overlaps a span has its box — expanded by [`BOX_PAD_PX`] and
/// clamped to the image — filled with `(B,G,R,A) = (0,0,0,255)`. Words that
/// hit nothing are left untouched, pixel for pixel.
///
/// A buffer whose length is not `width * height * 4` is painted only as far as
/// it reaches (never panics) and logged — the caller has handed over a frame
/// that does not match its own dimensions.
pub fn redact_bgra(
    bgra: &mut [u8],
    width: u32,
    height: u32,
    lines: &[LineBoxes],
    redactor: &Redactor,
) -> ImageRedactionReport {
    if bgra.len() != (width as usize) * (height as usize) * 4 {
        tracing::warn!(
            len = bgra.len(),
            width,
            height,
            "image redaction: buffer length does not match dimensions; painting what exists"
        );
    }
    let mut boxes_painted = 0usize;
    let mut counts: HashMap<RuleKind, u32> = HashMap::new();

    for line in lines {
        // Join with single spaces and remember each word's byte range.
        let mut joined = String::new();
        let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(line.words.len());
        for (i, word) in line.words.iter().enumerate() {
            if i > 0 {
                joined.push(' ');
            }
            let start = joined.len();
            joined.push_str(&word.text);
            ranges.push((start, joined.len()));
        }
        let spans = redactor.find_spans(&joined);
        if spans.is_empty() {
            continue;
        }
        for span in &spans {
            *counts.entry(span.rule).or_insert(0) += 1;
        }
        for (word, &(ws, we)) in line.words.iter().zip(&ranges) {
            let hit = spans.iter().any(|s| s.start < we && ws < s.end);
            if hit {
                paint_black(bgra, width, height, word);
                boxes_painted += 1;
            }
        }
    }

    let redactions = RuleKind::ORDERED
        .iter()
        .filter_map(|k| {
            counts
                .get(k)
                .filter(|&&n| n > 0)
                .map(|&n| Redaction { rule: k.label().to_string(), count: n })
        })
        .collect();
    ImageRedactionReport { boxes_painted, redactions }
}

/// Fill `word`'s box, expanded by [`BOX_PAD_PX`] and clamped to the image, with
/// opaque black. Pure index arithmetic; every row write is bounds-clamped to the
/// buffer so a short buffer can never panic.
fn paint_black(bgra: &mut [u8], width: u32, height: u32, word: &WordBox) {
    let x0 = word.x.saturating_sub(BOX_PAD_PX) as usize;
    let y0 = word.y.saturating_sub(BOX_PAD_PX) as usize;
    let x1 = (word.x.saturating_add(word.w).saturating_add(BOX_PAD_PX)).min(width) as usize;
    let y1 = (word.y.saturating_add(word.h).saturating_add(BOX_PAD_PX)).min(height) as usize;
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    let stride = width as usize * 4;
    for row in y0..y1 {
        let start = (row * stride + x0 * 4).min(bgra.len());
        let end = (row * stride + x1 * 4).min(bgra.len());
        for px in bgra[start..end].chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 0, 255]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 64;
    const H: u32 = 32;
    const FILL: u8 = 0x80;

    fn buffer() -> Vec<u8> {
        vec![FILL; (W * H * 4) as usize]
    }

    fn word(text: &str, x: u32, w: u32) -> WordBox {
        WordBox { text: text.into(), x, y: 8, w, h: 10 }
    }

    fn redactor() -> Redactor {
        Redactor::new(&[]).expect("built-in rules compile")
    }

    /// Every pixel inside `boxes` (already padded + clamped) is opaque black and
    /// every pixel outside is the untouched fill.
    fn assert_only_boxes_black(bgra: &[u8], boxes: &[(u32, u32, u32, u32)]) {
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                let px = &bgra[i..i + 4];
                let inside = boxes.iter().any(|&(x0, y0, x1, y1)| x >= x0 && x < x1 && y >= y0 && y < y1);
                if inside {
                    assert_eq!(px, &[0, 0, 0, 255], "px ({x},{y}) inside a box is opaque black");
                } else {
                    assert_eq!(px, &[FILL; 4], "px ({x},{y}) outside every box is untouched");
                }
            }
        }
    }

    #[test]
    fn only_the_secret_word_box_is_painted() {
        let mut bgra = buffer();
        // The secret's box runs to x=63 so the +2 px pad must clamp at the edge.
        let line = LineBoxes {
            words: vec![word("my", 2, 10), word("key", 14, 12), word("sk-abcdefghijklmnop1234", 30, 33)],
        };
        let report = redact_bgra(&mut bgra, W, H, &[line], &redactor());
        assert_eq!(report.boxes_painted, 1);
        assert_eq!(report.redactions.len(), 1);
        assert_eq!(report.redactions[0].rule, "secret_key");
        assert_eq!(report.redactions[0].count, 1);
        // Box (30,8)+(33,10) padded by 2 ⇒ x 28..65→64, y 6..20.
        assert_only_boxes_black(&bgra, &[(28, 6, 64, 20)]);
    }

    #[test]
    fn a_card_number_split_across_words_paints_all_four_boxes() {
        let mut bgra = buffer();
        let line = LineBoxes {
            words: vec![word("4111", 2, 10), word("1111", 16, 10), word("1111", 30, 10), word("1111", 44, 10)],
        };
        let report = redact_bgra(&mut bgra, W, H, &[line], &redactor());
        assert_eq!(report.boxes_painted, 4, "one Luhn-valid run ⇒ every word of it");
        assert_eq!(report.redactions.len(), 1);
        assert_eq!(report.redactions[0].rule, "payment_card");
        assert_eq!(report.redactions[0].count, 1, "one hit, four boxes");
        assert_only_boxes_black(
            &bgra,
            &[(0, 6, 14, 20), (14, 6, 28, 20), (28, 6, 42, 20), (42, 6, 56, 20)],
        );
    }

    #[test]
    fn clean_text_paints_nothing_and_leaves_the_buffer_unchanged() {
        let mut bgra = buffer();
        let lines = vec![
            LineBoxes { words: vec![word("hello", 2, 20), word("world", 26, 20)] },
            LineBoxes { words: vec![word("order", 2, 20), word("1234567890123", 26, 30)] },
            LineBoxes { words: vec![] },
        ];
        let report = redact_bgra(&mut bgra, W, H, &lines, &redactor());
        assert_eq!(report.boxes_painted, 0);
        assert!(report.redactions.is_empty());
        assert_eq!(bgra, buffer(), "not a single byte changed");
    }

    #[test]
    fn a_short_buffer_is_painted_as_far_as_it_reaches_without_panicking() {
        // Half the rows are missing: the box's lower rows fall off the end.
        let mut bgra = vec![FILL; (W * (H / 2) * 4) as usize];
        let line = LineBoxes { words: vec![word("a@b.com", 4, 20)] };
        let report = redact_bgra(&mut bgra, W, H, &[line], &redactor());
        assert_eq!(report.boxes_painted, 1);
        assert_eq!(report.redactions[0].rule, "email");
        // Rows 6..16 exist; all of them carry the box (x 2..26).
        for y in 6..(H / 2) {
            let i = ((y * W + 2) * 4) as usize;
            assert_eq!(&bgra[i..i + 4], &[0, 0, 0, 255]);
        }
    }
}
