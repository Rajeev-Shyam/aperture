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

use std::borrow::Cow;
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

/// One OCR line: its words (with boxes) in reading order, plus the line text
/// as the TEXT path sees it. With `text` empty the line text is derived from
/// the words joined by single spaces — so a card number OCR'd as four words is
/// still one Luhn-valid run. With `text` given, [`redact_bgra`] can tell
/// whether the geometry is COMPLETE (one word box per whitespace token) and
/// fail closed when a rule hits text that has no box (08-22 review).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineBoxes {
    /// The line text the text rules see; `""` ⇒ join the words.
    pub text: String,
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
    /// Lines a rule HIT whose geometry cannot be trusted to cover the hit: no
    /// word boxes at all (the engine's `Words()` failed) or fewer boxes than
    /// whitespace tokens (a word's rect failed). Their words are painted
    /// anyway as best effort, but pixels the text path redacts may still be
    /// readable — **the caller must withhold the frame when this is non-zero**
    /// (fail closed, 08-22 review).
    pub lines_unpaintable: usize,
}

/// Where one line landed in the joined document, and — when its geometry is
/// complete — each whitespace token's byte range, one per word.
struct LineMap {
    start: usize,
    end: usize,
    tokens: Vec<(usize, usize)>,
}

/// Paint opaque black over every OCR word the text rules would redact
/// (doc 24 decision #3).
///
/// `bgra` is a BGRA8 buffer of `width × height` px (the frame the OCR ran on,
/// so `lines` are in its pixel space). The line texts (each line's `text`, or
/// its words joined by single spaces) are joined into ONE document with
/// `'\n'` between lines — the same shape the OCR-text path redacts — and
/// [`Redactor::find_spans`] runs over that whole document, so a rule that
/// spans lines (a PEM/OPENSSH private-key body) paints every word it covers,
/// not just the header line (08-22 review). Any word whose token's byte range
/// overlaps a span has its box — expanded by [`BOX_PAD_PX`] and clamped to
/// the image — filled with `(B,G,R,A) = (0,0,0,255)`. Words that hit nothing
/// are left untouched, pixel for pixel.
///
/// **Fail closed on missing geometry (08-22 review):** a line a rule hit whose
/// word count differs from its token count (no words at all, or a word whose
/// rect the engine could not give) has every box it does have painted, and is
/// counted in [`ImageRedactionReport::lines_unpaintable`] — the caller must
/// then withhold the frame, because text the rule covers may have no box.
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
    let mut lines_unpaintable = 0usize;
    let mut counts: HashMap<RuleKind, u32> = HashMap::new();

    // One document, one pass: line texts joined with '\n', so multi-line
    // rules see the same text shape the OCR-text path redacts. Each line's
    // whitespace tokens are mapped to its words by position.
    let mut joined = String::new();
    let mut maps: Vec<LineMap> = Vec::with_capacity(lines.len());
    for (li, line) in lines.iter().enumerate() {
        if li > 0 {
            joined.push('\n');
        }
        let text: Cow<'_, str> = if line.text.trim().is_empty() {
            Cow::Owned(line.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "))
        } else {
            Cow::Borrowed(line.text.as_str())
        };
        let start = joined.len();
        joined.push_str(&text);
        let end = joined.len();
        let mut tokens = Vec::with_capacity(line.words.len());
        let mut cursor = 0usize;
        for tok in text.split_whitespace() {
            // Tokens come in order and cannot begin inside whitespace, so the
            // first occurrence at or after the cursor is this token.
            let pos = cursor + text[cursor..].find(tok).unwrap_or(0);
            tokens.push((start + pos, start + pos + tok.len()));
            cursor = pos + tok.len();
        }
        maps.push(LineMap { start, end, tokens });
    }
    let spans = redactor.find_spans(&joined);
    for span in &spans {
        *counts.entry(span.rule).or_insert(0) += 1;
    }
    if !spans.is_empty() {
        for (line, map) in lines.iter().zip(&maps) {
            let line_hit = spans.iter().any(|s| s.start < map.end && map.start < s.end);
            if !line_hit {
                continue;
            }
            if line.words.len() != map.tokens.len() {
                // Geometry incomplete: some text the rule covers has no box.
                // Paint what exists (best effort) and report the line so the
                // caller withholds the frame.
                lines_unpaintable += 1;
                for word in &line.words {
                    paint_black(bgra, width, height, word);
                    boxes_painted += 1;
                }
                continue;
            }
            for (word, &(ws, we)) in line.words.iter().zip(&map.tokens) {
                let hit = spans.iter().any(|s| s.start < we && ws < s.end);
                if hit {
                    paint_black(bgra, width, height, word);
                    boxes_painted += 1;
                }
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
    ImageRedactionReport { boxes_painted, redactions, lines_unpaintable }
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
            text: String::new(),
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
            text: String::new(),
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
            LineBoxes { text: String::new(), words: vec![word("hello", 2, 20), word("world", 26, 20)] },
            LineBoxes { text: String::new(), words: vec![word("order", 2, 20), word("1234567890123", 26, 30)] },
            LineBoxes { text: String::new(), words: vec![] },
        ];
        let report = redact_bgra(&mut bgra, W, H, &lines, &redactor());
        assert_eq!(report.boxes_painted, 0);
        assert!(report.redactions.is_empty());
        assert_eq!(bgra, buffer(), "not a single byte changed");
    }

    /// 08-22 review [high]: a PEM body spans lines — the text rule redacts
    /// the whole thing, so the image gate must paint every line's words, not
    /// just the BEGIN header's.
    #[test]
    fn a_multi_line_pem_key_paints_every_line_it_covers() {
        let mut bgra = buffer();
        let lines = vec![
            LineBoxes { text: String::new(), words: vec![word("-----BEGIN", 2, 12), word("OPENSSH", 18, 10), word("PRIVATE", 32, 10), word("KEY-----", 46, 10)] },
            LineBoxes { text: String::new(), words: vec![WordBox { text: "b3BlbnNzaC1rZXktdjEAAAAA".into(), x: 2, y: 20, w: 40, h: 8 }] },
            LineBoxes { text: String::new(), words: vec![
                WordBox { text: "-----END".into(), x: 2, y: 29, w: 10, h: 2 },
                WordBox { text: "OPENSSH".into(), x: 16, y: 29, w: 10, h: 2 },
                WordBox { text: "PRIVATE".into(), x: 30, y: 29, w: 10, h: 2 },
                WordBox { text: "KEY-----".into(), x: 44, y: 29, w: 10, h: 2 },
            ] },
        ];
        let report = redact_bgra(&mut bgra, W, H, &lines, &redactor());
        assert_eq!(report.boxes_painted, 9, "every word of all three lines");
        assert_eq!(report.redactions.len(), 1);
        assert_eq!(report.redactions[0].rule, "secret_key");
        assert_eq!(report.redactions[0].count, 1, "one span across the lines");
        // The middle line's body word is painted (the old per-line pass left it).
        let i = ((22 * W + 10) * 4) as usize;
        assert_eq!(&bgra[i..i + 4], &[0, 0, 0, 255], "body line painted");
    }

    /// An explicit line `text` equal to the words changes nothing: same spans,
    /// same boxes, `lines_unpaintable == 0`.
    #[test]
    fn explicit_line_text_with_complete_geometry_paints_exactly_the_hit_words() {
        let mut bgra = buffer();
        let line = LineBoxes {
            text: "my key  sk-abcdefghijklmnop1234".into(), // double space: tokens, not bytes, are mapped
            words: vec![word("my", 2, 10), word("key", 14, 12), word("sk-abcdefghijklmnop1234", 30, 33)],
        };
        let report = redact_bgra(&mut bgra, W, H, &[line], &redactor());
        assert_eq!(report.boxes_painted, 1);
        assert_eq!(report.lines_unpaintable, 0);
        assert_only_boxes_black(&bgra, &[(28, 6, 64, 20)]);
    }

    /// 08-22 review [low]: the engine's `Words()` failed for a line — text,
    /// no boxes. A rule hit on it has nothing to paint: the line is reported
    /// unpaintable so the caller withholds the frame (text-only step).
    #[test]
    fn a_hit_line_without_geometry_is_reported_unpaintable() {
        let mut bgra = buffer();
        let lines = vec![
            LineBoxes { text: "hello world".into(), words: vec![] },
            LineBoxes { text: "token sk-abcdefghijklmnop1234 ok".into(), words: vec![] },
        ];
        let report = redact_bgra(&mut bgra, W, H, &lines, &redactor());
        assert_eq!(report.lines_unpaintable, 1, "only the line a rule hit");
        assert_eq!(report.boxes_painted, 0);
        assert_eq!(report.redactions[0].rule, "secret_key");
        assert_eq!(bgra, buffer(), "nothing to paint");
    }

    /// 08-22 review [low]: one group of a card number lost its rect. The word
    /// document alone (12 digits) would fail the Luhn gate and paint NOTHING;
    /// the line text (16 digits) hits, the geometry is incomplete, so every
    /// surviving box is painted AND the line is reported unpaintable.
    #[test]
    fn a_hit_line_with_a_missing_word_box_is_painted_best_effort_and_reported() {
        let mut bgra = buffer();
        let line = LineBoxes {
            text: "4111 1111 1111 1111".into(),
            words: vec![word("4111", 2, 10), word("1111", 16, 10), word("1111", 30, 10)],
        };
        let report = redact_bgra(&mut bgra, W, H, &[line], &redactor());
        assert_eq!(report.redactions[0].rule, "payment_card");
        assert_eq!(report.lines_unpaintable, 1);
        assert_eq!(report.boxes_painted, 3, "every box that exists");
        assert_only_boxes_black(&bgra, &[(0, 6, 14, 20), (14, 6, 28, 20), (28, 6, 42, 20)]);
    }

    /// Incomplete geometry on a line NO rule hits is not a problem: nothing to
    /// cover, nothing to withhold.
    #[test]
    fn a_clean_line_without_geometry_is_not_reported() {
        let mut bgra = buffer();
        let lines = vec![
            LineBoxes { text: "hello world".into(), words: vec![] },
            LineBoxes { text: "order 42".into(), words: vec![word("order", 2, 20)] },
        ];
        let report = redact_bgra(&mut bgra, W, H, &lines, &redactor());
        assert_eq!(report.lines_unpaintable, 0);
        assert_eq!(report.boxes_painted, 0);
        assert_eq!(bgra, buffer());
    }

    #[test]
    fn a_short_buffer_is_painted_as_far_as_it_reaches_without_panicking() {
        // Half the rows are missing: the box's lower rows fall off the end.
        let mut bgra = vec![FILL; (W * (H / 2) * 4) as usize];
        let line = LineBoxes { text: String::new(), words: vec![word("a@b.com", 4, 20)] };
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
