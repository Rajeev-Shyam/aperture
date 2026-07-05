//! Text embedding (doc 03 §5, doc 06 §2, doc 04 §7-8).
//!
//! **Text is the context currency, not screenshots** (doc 06 §2): the cheap
//! always-on OCR text is embedded here and written into the `ctx_vec` virtual
//! table (768-d, doc 03 §3) so the voice-query retrieval path (doc 03 §5) and
//! the pattern engine's semantic assist (doc 08 §5) can do KNN / cosine work —
//! all fully local, no GPU, no network.
//!
//! Model: `nomic-embed-text-v1.5` (137M params), pinned to **768 dimensions**
//! (the model is Matryoshka 64..768; doc 03 pins the full 768 so `ctx_vec` and
//! every stored vector agree). It runs on **CPU**, ~520 MB resident (doc 04 §7),
//! within a **<= 300 ms** per-embed budget (doc 04 §8, Critical Path A step 5).
//!
//! ## Backends
//! [VERIFY resolved — M2]: the build prompt suggested `llama-cpp-2`, but that
//! crate needs cmake + libclang (bindgen) on MSVC — friction this box doesn't
//! have. The swappable-trait seam absorbs the change (doc 06 §2 spirit):
//! - **`NomicEmbedder`** (feature `nomic`, default ON since 2026-07-05 — the
//!   weights are fetched into the repo's `models/`): `fastembed` (ONNX
//!   Runtime, CPU) serving nomic-embed-text-v1.5 with the proper
//!   `search_document:` / `search_query:` task prefixes. First construction
//!   downloads the model if missing (`examples/fetch_model.rs` is the
//!   documented setup step); `--no-default-features` restores the
//!   zero-download dev path.
//! - **`HashEmbedder`** (always available): a deterministic char-3-gram
//!   feature-hash embedding. NOT semantic — it exists so the full pipeline
//!   (store → ctx_vec → KNN) runs end-to-end in dev/tests with zero downloads.
//!   Exact-duplicate/near-duplicate text still lands near itself, so the M2
//!   KNN sanity gate is meaningful; the semantic-quality gate requires `nomic`.
//!
//! This is a Tier-0 crate: CPU-only, never touches the GPU mutex (doc 12) and
//! never opens a socket itself (doc 13 §2) — the one exception is `fastembed`'s
//! first-run model download, which is an OPT-IN build feature and a documented
//! setup step (like the GGUF fetches in `models/`), not a runtime egress path.

/// Embedding dimensionality, pinned to `nomic-embed-text-v1.5` and the `ctx_vec`
/// `float[768]` column (doc 03 §3). Changing this is a schema-breaking change
/// requiring a migration (doc 15 §6) — it is intentionally a hard constant.
pub const EMBED_DIM: usize = 768;

/// Resident-RAM budget for the loaded model (doc 04 §7), recorded as a measured
/// number at the M2 gate (doc 16).
pub const EMBED_MODEL_RESIDENT_MB: u32 = 520;

/// The task prefix nomic-embed expects for stored documents (doc 03 §5).
pub const DOC_PREFIX: &str = "search_document: ";
/// The task prefix nomic-embed expects for queries (doc 03 §5 step 1).
pub const QUERY_PREFIX: &str = "search_query: ";

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("failed to load embedding model: {0}")]
    ModelLoad(String),
    #[error("tokenization failed: {0}")]
    Tokenize(String),
    #[error("inference failed: {0}")]
    Inference(String),
}

/// A text embedder. Implementations are CPU-only and produce a fixed
/// [`EMBED_DIM`]-length, L2-normalized vector. Kept as a trait so the runtime
/// backend is swappable without touching callers (store, retrieval, patterns).
pub trait Embedder: Send + Sync {
    /// Embed one piece of text into a pinned 768-d vector.
    ///
    /// Budget: **<= 300 ms** on one CPU core (doc 04 §8). The result is suitable
    /// for direct insertion into `ctx_vec` and for cosine comparison against
    /// pattern centroids (doc 08 §5). The same backend must embed both stored
    /// text and queries (doc 03 §5 step 1) so distances are comparable.
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError>;

    /// Stable backend id for telemetry / the M2 gate report.
    fn id(&self) -> &'static str;
}

// ---------------------------------------------------------------------------
// HashEmbedder — deterministic, dependency-free, NOT semantic (see module doc).
// ---------------------------------------------------------------------------

/// Deterministic char-trigram feature-hash embedding (768-d, L2-normalized).
///
/// Dev/test backend only: it preserves *lexical* similarity (shared trigrams),
/// not semantics. The M2 pipeline gate runs on it; semantic retrieval quality
/// is asserted only with the `nomic` feature enabled.
pub struct HashEmbedder;

impl Embedder for HashEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let mut vec = vec![0f32; EMBED_DIM];
        let normalized: String = text
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect();
        let chars: Vec<char> = normalized.chars().collect();
        if chars.len() >= 3 {
            for w in chars.windows(3) {
                if w.iter().all(|c| *c == ' ') {
                    continue;
                }
                let mut h = 1469598103934665603u64; // FNV-1a
                for c in w {
                    h ^= *c as u64;
                    h = h.wrapping_mul(1099511628211);
                }
                let idx = (h % EMBED_DIM as u64) as usize;
                let sign = if (h >> 63) == 0 { 1.0 } else { -1.0 };
                vec[idx] += sign;
            }
        }
        // L2-normalize (cosine-comparable, like the real model's output).
        let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vec {
                *v /= norm;
            }
        }
        Ok(vec)
    }

    fn id(&self) -> &'static str {
        "hash-trigram-768 (dev only, not semantic)"
    }
}

// ---------------------------------------------------------------------------
// NomicEmbedder — the real model (feature `nomic`, default OFF; see module doc).
// ---------------------------------------------------------------------------

/// The default Tier-0 embedder: `nomic-embed-text-v1.5`, 137M, CPU, 768-d,
/// via fastembed/ONNX Runtime. Loading is a one-time cost; `embed` reuses the
/// session. First construction downloads the model — opt-in only.
#[cfg(feature = "nomic")]
pub struct NomicEmbedder {
    model: std::sync::Mutex<fastembed::TextEmbedding>,
}

#[cfg(feature = "nomic")]
impl NomicEmbedder {
    /// Load nomic-embed-text-v1.5 on CPU. `cache_dir` is where the ONNX weights
    /// live (e.g. the repo's git-ignored `models/`); first run downloads there.
    pub fn load(cache_dir: std::path::PathBuf) -> Result<Self, EmbedError> {
        use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::NomicEmbedTextV15)
                .with_cache_dir(cache_dir)
                .with_show_download_progress(true),
        )
        .map_err(|e| EmbedError::ModelLoad(e.to_string()))?;
        Ok(Self { model: std::sync::Mutex::new(model) })
    }
}

#[cfg(feature = "nomic")]
impl Embedder for NomicEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        // nomic task prefix: stored OCR text is a document (doc 03 §5); the
        // voice-retrieval path wraps queries with QUERY_PREFIX before calling.
        let input = if text.starts_with(QUERY_PREFIX) || text.starts_with(DOC_PREFIX) {
            text.to_string()
        } else {
            format!("{DOC_PREFIX}{text}")
        };
        let mut model = self.model.lock().expect("embedder lock");
        let mut out = model
            .embed(vec![input], None)
            .map_err(|e| EmbedError::Inference(e.to_string()))?;
        let vec = out.pop().ok_or_else(|| EmbedError::Inference("empty batch".into()))?;
        if vec.len() != EMBED_DIM {
            return Err(EmbedError::Inference(format!(
                "backend returned {} dims, ctx_vec is pinned to {EMBED_DIM} (doc 03 §3)",
                vec.len()
            )));
        }
        Ok(vec)
    }

    fn id(&self) -> &'static str {
        "nomic-embed-text-v1.5 (fastembed/onnx, cpu)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_embedder_is_deterministic_normalized_and_768d() {
        let e = HashEmbedder;
        let a = e.embed("continue the rust tutorial video").unwrap();
        let b = e.embed("continue the rust tutorial video").unwrap();
        assert_eq!(a.len(), EMBED_DIM);
        assert_eq!(a, b, "deterministic");
        let norm: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "L2-normalized (got {norm})");
    }

    #[test]
    fn lexically_similar_text_is_nearer_than_unrelated_text() {
        let e = HashEmbedder;
        let a = e.embed("quarterly budget spreadsheet totals").unwrap();
        let b = e.embed("the quarterly budget spreadsheet").unwrap();
        let c = e.embed("kernel scheduler preemption latency").unwrap();
        let cos = |x: &[f32], y: &[f32]| -> f32 { x.iter().zip(y).map(|(a, b)| a * b).sum() };
        assert!(
            cos(&a, &b) > cos(&a, &c),
            "shared trigrams rank closer (M2 sanity shape)"
        );
    }
}
