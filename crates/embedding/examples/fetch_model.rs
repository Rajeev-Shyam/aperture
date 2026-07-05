//! One-time model fetch + sanity check (the documented setup step from the
//! module doc): downloads nomic-embed-text-v1.5 (~250 MB) into the repo's
//! git-ignored `models/` dir and embeds one string to prove the backend works.
//!
//! Run from the repo root:
//! `cargo run -p aperture-embedding --features nomic --example fetch_model`

#[cfg(feature = "nomic")]
fn main() {
    use aperture_embedding::{Embedder, NomicEmbedder, EMBED_DIM};
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "models".to_string());
    println!("fetching nomic-embed-text-v1.5 into {dir}/ (first run ~250 MB)…");
    let embedder = NomicEmbedder::load(std::path::PathBuf::from(&dir)).expect("model load");
    let started = std::time::Instant::now();
    let v = embedder
        .embed("continue the rust tutorial video")
        .expect("embed");
    println!(
        "OK: backend `{}`, {} dims (pinned {EMBED_DIM}), embed took {:?}",
        embedder.id(),
        v.len(),
        started.elapsed()
    );
}

#[cfg(not(feature = "nomic"))]
fn main() {
    eprintln!("build with --features nomic (this example IS the model fetch)");
    std::process::exit(1);
}
