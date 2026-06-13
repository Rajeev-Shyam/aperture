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
//! This is a Tier-0 crate: CPU-only, never touches the GPU mutex (doc 12) and
//! never opens a socket (doc 13 §2 — only the reasoning gateway may).

use std::path::PathBuf;

/// Embedding dimensionality, pinned to `nomic-embed-text-v1.5` and the `ctx_vec`
/// `float[768]` column (doc 03 §3). Changing this is a schema-breaking change
/// requiring a migration (doc 15 §6) — it is intentionally a hard constant.
pub const EMBED_DIM: usize = 768;

/// Resident-RAM budget for the loaded model (doc 04 §7), recorded as a measured
/// number at the M2 gate (doc 16).
// TODO(M2:) replace with the measured resident figure at the M2 gate.
pub const EMBED_MODEL_RESIDENT_MB: u32 = 520;

/// A text embedder. Implementations are CPU-only and produce a fixed
/// [`EMBED_DIM`]-length vector. Kept as a trait so the runtime backend (the M2
/// decision) is swappable without touching callers (store, retrieval, patterns).
pub trait Embedder: Send + Sync {
    /// Embed one piece of text into a pinned 768-d vector.
    ///
    /// Budget: **<= 300 ms** on one CPU core (doc 04 §8). The result is suitable
    /// for direct insertion into `ctx_vec` and for cosine comparison against
    /// pattern centroids (doc 08 §5). Implementations normalize as the model
    /// requires; the same model must embed both stored text and queries
    /// (doc 03 §5 step 1) so distances are comparable.
    fn embed(&self, text: &str) -> [f32; EMBED_DIM];
}

/// The default Tier-0 embedder: `nomic-embed-text-v1.5`, 137M, CPU, 768-d.
pub struct NomicEmbedder {
    // model: <backend handle>,      // TODO(M2:) candle/ort/gguf session [VERIFY]
    // tokenizer: <tokenizer>,       // TODO(M2:) HF tokenizer for nomic [VERIFY]
}

impl NomicEmbedder {
    /// Load the GGUF/safetensors weights + tokenizer from disk and prepare the
    /// CPU session. Loading is a one-time cost; `embed` reuses the session.
    pub fn load(_model_path: PathBuf) -> Result<Self, EmbedError> {
        // TODO(M2:) load nomic-embed-text-v1.5 weights + tokenizer on CPU; pin
        // output to EMBED_DIM (Matryoshka truncate -> normalize); enforce the
        // <=300ms budget at the M2 gate (doc 16). [VERIFY backend]
        todo!("M2: load nomic-embed-text-v1.5 (137M, CPU, 768-d) + tokenizer")
    }
}

impl Embedder for NomicEmbedder {
    fn embed(&self, _text: &str) -> [f32; EMBED_DIM] {
        // TODO(M2:) tokenize (apply nomic's `search_document:` / `search_query:`
        // task prefix as appropriate — doc 03 §5), forward pass on CPU, mean-pool,
        // L2-normalize, truncate to EMBED_DIM.
        todo!("M2: tokenize + CPU forward pass + pool + normalize to 768-d")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("failed to load embedding model: {0}")]
    ModelLoad(String),
    #[error("tokenization failed: {0}")]
    Tokenize(String),
    #[error("inference failed: {0}")]
    Inference(String),
}
