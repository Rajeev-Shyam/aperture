//! Perceptual-hash near-duplicate gate (ADR-032/Q72, doc 05 §4, Doc 21 §2).
//!
//! Sits **before** OCR/embed: when a new frame's hash is within a Hamming
//! threshold of the previous frame's, the frame is redundant (static screen) and
//! OCR + embedding are skipped. The gate only *removes* work — it can never
//! delay a bubble (Doc 21 FIX/§2 wording).
//!
//! Algorithm `[ASSUMPTION — tuned at M2]`: 64-bit **difference hash** (dHash)
//! over a 9×8 grayscale downsample — each bit is `left pixel > right pixel`.
//! dHash is gradient-based, cheap (no DCT), and robust to uniform brightness
//! shifts; if M2 tuning shows too many false skips on UI text, swap to a DCT
//! pHash behind the same function signature. The stored form (`thumb_phash`,
//! doc 03 §3) is the 16-hex-char string of the 64-bit value.

/// Default Hamming threshold (bits) under which two frames count as duplicates.
/// `[ASSUMPTION — start at 4, tuned at M2]` (mirrors `CaptureConfig::phash_hamming_threshold`).
pub const DEFAULT_HAMMING_THRESHOLD: u32 = 4;

/// 64-bit dHash over an 8-bit grayscale image.
///
/// `gray` is row-major, `width`×`height`, one byte per pixel. Works on any
/// input ≥ 9×8; the downsample is a box mean over the source cells.
pub fn dhash64_gray(gray: &[u8], width: usize, height: usize) -> u64 {
    debug_assert_eq!(gray.len(), width * height, "gray buffer size");
    if width < 9 || height < 8 || gray.len() < width * height {
        return 0; // degenerate frame: hash 0 (never gates, doc 21 §2 — only removes work)
    }
    // Downsample to 9×8 by box-averaging source cells.
    let mut cells = [[0u32; 9]; 8];
    for (cy, row) in cells.iter_mut().enumerate() {
        let y0 = cy * height / 8;
        let y1 = ((cy + 1) * height / 8).max(y0 + 1);
        for (cx, cell) in row.iter_mut().enumerate() {
            let x0 = cx * width / 9;
            let x1 = ((cx + 1) * width / 9).max(x0 + 1);
            let mut sum = 0u32;
            let mut n = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += gray[y * width + x] as u32;
                    n += 1;
                }
            }
            *cell = sum / n.max(1);
        }
    }
    // Each bit: left cell brighter than its right neighbour.
    let mut hash = 0u64;
    for (cy, row) in cells.iter().enumerate() {
        for cx in 0..8 {
            if row[cx] > row[cx + 1] {
                hash |= 1u64 << (cy * 8 + cx);
            }
        }
    }
    hash
}

/// 64-bit dHash over a BGRA8 buffer (the WGC staging format, doc 05 §2):
/// converts to luma then hashes.
pub fn dhash64_bgra(bgra: &[u8], width: usize, height: usize) -> u64 {
    debug_assert_eq!(bgra.len(), width * height * 4, "bgra buffer size");
    let mut gray = vec![0u8; width * height];
    for (i, px) in bgra.chunks_exact(4).enumerate() {
        // Integer Rec.601 luma: (29·B + 150·G + 77·R) >> 8.
        let (b, g, r) = (px[0] as u32, px[1] as u32, px[2] as u32);
        gray[i] = ((29 * b + 150 * g + 77 * r) >> 8) as u8;
    }
    dhash64_gray(&gray, width, height)
}

/// Hamming distance between two 64-bit hashes.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// The stored `thumb_phash` string form (doc 03 §3): 16 lowercase hex chars.
pub fn to_hex(hash: u64) -> String {
    format!("{hash:016x}")
}

/// The stateful gate: remembers the last frame's hash and answers "is this new
/// frame a near-duplicate?" (doc 05 §4). One instance per capture pipeline.
#[derive(Debug, Default)]
pub struct NearDuplicateGate {
    last: Option<u64>,
    threshold: u32,
    /// Frames suppressed since start (gate telemetry for the M2 tuning).
    pub suppressed: u64,
}

impl NearDuplicateGate {
    pub fn new(threshold: u32) -> Self {
        Self { last: None, threshold, suppressed: 0 }
    }

    /// Check a new frame's hash. Returns `true` when the frame is a near-dup of
    /// the previous one (⇒ skip OCR/embed); always updates the remembered hash.
    pub fn is_duplicate(&mut self, hash: u64) -> bool {
        let dup = match self.last {
            Some(prev) => hamming(prev, hash) <= self.threshold,
            None => false,
        };
        self.last = Some(hash);
        if dup {
            self.suppressed += 1;
        }
        dup
    }

    /// Forget the last hash (toggle OFF→ON or session boundary): the next frame
    /// is never suppressed.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_frame(width: usize, height: usize, step: u8) -> Vec<u8> {
        (0..width * height)
            .map(|i| ((i % width) as u8).wrapping_mul(step))
            .collect()
    }

    #[test]
    fn identical_frames_are_duplicates_and_distinct_ones_are_not() {
        let w = 90;
        let h = 80;
        let a = gradient_frame(w, h, 2);
        // A very different frame: inverted gradient.
        let b: Vec<u8> = a.iter().map(|&v| 255 - v).collect();

        let ha = dhash64_gray(&a, w, h);
        let hb = dhash64_gray(&b, w, h);
        assert_eq!(hamming(ha, ha), 0);
        assert!(
            hamming(ha, hb) > DEFAULT_HAMMING_THRESHOLD,
            "inverted gradient must differ (got {})",
            hamming(ha, hb)
        );

        let mut gate = NearDuplicateGate::new(DEFAULT_HAMMING_THRESHOLD);
        assert!(!gate.is_duplicate(ha), "first frame never suppressed");
        assert!(gate.is_duplicate(ha), "identical frame suppressed");
        assert!(!gate.is_duplicate(hb), "changed screen passes");
        assert_eq!(gate.suppressed, 1);
    }

    #[test]
    fn small_noise_stays_within_threshold() {
        let w = 90;
        let h = 80;
        let a = gradient_frame(w, h, 2);
        // Tiny perturbation: bump a handful of pixels.
        let mut b = a.clone();
        for i in (0..b.len()).step_by(1013) {
            b[i] = b[i].saturating_add(3);
        }
        let d = hamming(dhash64_gray(&a, w, h), dhash64_gray(&b, w, h));
        assert!(d <= DEFAULT_HAMMING_THRESHOLD, "cursor-blink-level noise gated (d={d})");
    }

    #[test]
    fn bgra_path_matches_gray_path_on_gray_input() {
        let w = 45;
        let h = 40;
        let gray = gradient_frame(w, h, 3);
        let mut bgra = Vec::with_capacity(gray.len() * 4);
        for &g in &gray {
            bgra.extend_from_slice(&[g, g, g, 255]);
        }
        // Same ordering relationships ⇒ identical dHash.
        assert_eq!(dhash64_gray(&gray, w, h), dhash64_bgra(&bgra, w, h));
    }

    #[test]
    fn degenerate_frames_hash_to_zero_and_never_gate() {
        assert_eq!(dhash64_gray(&[0u8; 4], 2, 2), 0);
        let mut gate = NearDuplicateGate::new(4);
        assert!(!gate.is_duplicate(0));
        gate.reset();
        assert!(!gate.is_duplicate(0), "reset forgets history");
    }

    #[test]
    fn hex_form_is_16_chars() {
        assert_eq!(to_hex(0), "0000000000000000");
        assert_eq!(to_hex(u64::MAX), "ffffffffffffffff");
    }
}
