//! VLM weight ingress (decision #30): the explicit, user-initiated download of
//! the Qwen2.5-VL GGUF weights, which cannot ship in the installer (NSIS 2 GB
//! cap; ~3.3 GB total) and previously existed only as manual hardlinks on the
//! dev machine — every other machine silently degraded to OCR-only.
//!
//! **Two-emitter rule note (doc 13 §2):** this module opens an outbound HTTPS
//! connection, which is why it lives in `orchestration` — a lint-emitters
//! SANCTIONED crate — and not in the shell. It is model **ingress**, not data
//! egress: the request carries only the artifact URL and a resume byte offset,
//! never user data, and it runs ONLY from the Dashboard's explicit "Download"
//! click (`vlm_download` command) — never automatically. The precedent is the
//! embedder's opt-in fastembed first-run fetch (crates/embedding module doc);
//! the URLs + expected sizes come from settings, never code (NG8).
//!
//! Mechanics per artifact: stream to `<dest>.part`, resume with a `Range`
//! request when a previous attempt left a shorter `.part`, verify the final
//! byte count against the settings-declared size, then rename into place.
//! `SidecarConfig` resolves to these same destination paths even while the
//! files are absent (`main::sidecar_config`), so the very next VLM spawn picks
//! the weights up — no restart.

use std::path::{Path, PathBuf};

/// One weight artifact to fetch: where from, where to, and how many bytes the
/// finished file MUST be (both the "already installed?" check and the
/// post-download verification key on this size).
#[derive(Debug, Clone)]
pub struct FetchItem {
    pub url: String,
    pub dest: PathBuf,
    pub expected_bytes: u64,
}

/// Progress snapshot handed to the caller's callback. `received_bytes` /
/// `total_bytes` are OVERALL across every item (already-present files and a
/// resumed `.part` count as received), so one progress bar renders directly.
#[derive(Debug, Clone)]
pub struct FetchProgress {
    /// Destination file name of the artifact currently downloading.
    pub file: String,
    /// 0-based index of that artifact among the items.
    pub file_index: usize,
    pub file_count: usize,
    pub received_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("download failed: {0}")]
    Http(String),
    #[error("could not write the model file: {0}")]
    Io(String),
    /// The finished byte count disagrees with the settings-declared size —
    /// truncated transfer or a changed upstream file. The `.part` is deleted
    /// so the next attempt is a clean retry, never a corrupt rename.
    #[error("{file}: got {actual} bytes, expected {expected} — partial data discarded, retry the download")]
    SizeMismatch { file: String, expected: u64, actual: u64 },
}

/// Is this artifact already installed? Present means "exists AND is exactly
/// the expected size" — a wrong-size file is a truncated/corrupt download and
/// must read as missing, or the sidecar would spawn against garbage.
pub fn is_present(item: &FetchItem) -> bool {
    std::fs::metadata(&item.dest)
        .map(|m| m.len() == item.expected_bytes)
        .unwrap_or(false)
}

/// The subset of `items` not yet installed (see [`is_present`]).
pub fn missing(items: &[FetchItem]) -> Vec<FetchItem> {
    items.iter().filter(|i| !is_present(i)).cloned().collect()
}

/// `<dest>.part` — the in-flight download target. Appended, not
/// `with_extension` (which would REPLACE `.gguf`), so the partial file can
/// never collide with the real artifact name.
fn part_path(dest: &Path) -> PathBuf {
    let mut os = dest.as_os_str().to_owned();
    os.push(".part");
    PathBuf::from(os)
}

/// Fetch every not-yet-present item, streaming progress through `on_progress`.
///
/// Idempotent: already-present items are skipped (their bytes count as
/// received immediately), so a partial install resumes where it left off.
/// Terminal states are crisp: `Ok(())` means every item is in place at its
/// expected size; any `Err` leaves either a resumable `.part` (transport
/// error) or a clean slate (size mismatch) — never a wrong-size final file.
pub async fn fetch_all<F: FnMut(FetchProgress)>(
    items: &[FetchItem],
    on_progress: &mut F,
) -> Result<(), FetchError> {
    let total_bytes: u64 = items.iter().map(|i| i.expected_bytes).sum();
    // No overall timeout: this is a multi-GB transfer. The connect timeout
    // still bounds a dead network, and a stalled stream errors out of `chunk`.
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| FetchError::Http(e.to_string()))?;

    let mut done_bytes: u64 = 0;
    for (index, item) in items.iter().enumerate() {
        if is_present(item) {
            done_bytes += item.expected_bytes;
            continue;
        }
        fetch_one(&client, item, index, items.len(), done_bytes, total_bytes, on_progress).await?;
        done_bytes += item.expected_bytes;
    }
    Ok(())
}

/// Download one artifact to `<dest>.part`, resume-aware, verify, rename.
async fn fetch_one<F: FnMut(FetchProgress)>(
    client: &reqwest::Client,
    item: &FetchItem,
    file_index: usize,
    file_count: usize,
    done_bytes: u64,
    total_bytes: u64,
    on_progress: &mut F,
) -> Result<(), FetchError> {
    use tokio::io::AsyncWriteExt;

    let file_name = item
        .dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| item.dest.display().to_string());
    if let Some(parent) = item.dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| FetchError::Io(e.to_string()))?;
    }
    let part = part_path(&item.dest);

    // Resume-or-clean-retry: a shorter `.part` resumes via Range; one at or
    // past the expected size is a stale/corrupt leftover — start over.
    let mut offset = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    if offset >= item.expected_bytes {
        let _ = tokio::fs::remove_file(&part).await;
        offset = 0;
    }

    let progress = |received: u64| FetchProgress {
        file: file_name.clone(),
        file_index,
        file_count,
        received_bytes: done_bytes + received,
        total_bytes,
    };
    on_progress(progress(offset));

    let mut request = client.get(&item.url);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let mut resp = request
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| FetchError::Http(e.to_string()))?;
    // 206 = the server honored the resume; a plain 200 restarts from zero
    // (truncate — appending a full body to a partial file would corrupt it).
    let resuming = offset > 0 && resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if !resuming {
        offset = 0;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(resuming)
        .write(true)
        .truncate(!resuming)
        .open(&part)
        .await
        .map_err(|e| FetchError::Io(e.to_string()))?;

    let mut written = offset;
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            // Transport drop mid-stream: keep the `.part` (flushed below is
            // best-effort; the length on disk is what the next resume reads).
            Err(e) => {
                let _ = file.flush().await;
                return Err(FetchError::Http(e.to_string()));
            }
        };
        file.write_all(&chunk)
            .await
            .map_err(|e| FetchError::Io(e.to_string()))?;
        written += chunk.len() as u64;
        if written > item.expected_bytes {
            // The upstream file is bigger than the settings say — a changed
            // artifact. Never rename it into place; clean up for a fresh look.
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(FetchError::SizeMismatch {
                file: file_name,
                expected: item.expected_bytes,
                actual: written,
            });
        }
        on_progress(progress(written));
    }
    file.flush().await.map_err(|e| FetchError::Io(e.to_string()))?;
    drop(file);

    if written != item.expected_bytes {
        // Short body with a clean EOF (no transport error to resume from):
        // discard rather than leave a `.part` that would resume into the same
        // truncated upstream object.
        let _ = tokio::fs::remove_file(&part).await;
        return Err(FetchError::SizeMismatch {
            file: file_name,
            expected: item.expected_bytes,
            actual: written,
        });
    }

    // Atomic placement: the real name appears only once the bytes are whole.
    // A pre-existing wrong-size dest (corrupt earlier install) is replaced —
    // Windows rename refuses to overwrite, so remove it first.
    if tokio::fs::metadata(&item.dest).await.is_ok() {
        tokio::fs::remove_file(&item.dest)
            .await
            .map_err(|e| FetchError::Io(e.to_string()))?;
    }
    tokio::fs::rename(&part, &item.dest)
        .await
        .map_err(|e| FetchError::Io(e.to_string()))?;
    tracing::info!(file = %file_name, bytes = written, "VLM artifact installed (decision #30)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    /// A unique scratch dir per test (no tempfile dep in this workspace).
    fn scratch_dir(tag: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aperture-model-fetch-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What the fake loopback origin records about each request: the Range
    /// offset it was asked to resume from (None = full-file request).
    type SeenRanges = Arc<Mutex<Vec<Option<u64>>>>;

    /// A loopback HTTP origin serving one byte blob with Range support — the
    /// fake stand-in for the HF CDN (tests must NEVER touch the real 3 GB
    /// download). `serve_len` caps the bytes actually sent so a truncated
    /// upstream can be simulated.
    async fn spawn_origin(data: Vec<u8>, serve_len: usize, honor_range: bool) -> (String, SeenRanges) {
        use axum::http::{header, HeaderMap, StatusCode};
        let seen: SeenRanges = Arc::new(Mutex::new(Vec::new()));
        let seen_h = Arc::clone(&seen);
        let handler = move |headers: HeaderMap| {
            let data = data.clone();
            let seen = Arc::clone(&seen_h);
            async move {
                let range_start = headers
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.strip_prefix("bytes=")?.split('-').next()?.parse::<u64>().ok());
                seen.lock().unwrap().push(range_start);
                let body_end = serve_len.min(data.len());
                match range_start.filter(|_| honor_range) {
                    Some(start) => {
                        let start = (start as usize).min(body_end);
                        (StatusCode::PARTIAL_CONTENT, data[start..body_end].to_vec())
                    }
                    None => (StatusCode::OK, data[..body_end].to_vec()),
                }
            }
        };
        let app = axum::Router::new().route("/weights.gguf", axum::routing::get(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/weights.gguf", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, seen)
    }

    fn blob(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn presence_is_keyed_on_exact_size_not_mere_existence() {
        let dir = scratch_dir("present");
        let dest = dir.join("w.gguf");
        let item = FetchItem { url: String::new(), dest: dest.clone(), expected_bytes: 8 };
        assert!(!is_present(&item), "absent file is missing");
        std::fs::write(&dest, b"12345678").unwrap();
        assert!(is_present(&item), "exact-size file is installed");
        std::fs::write(&dest, b"1234").unwrap();
        assert!(!is_present(&item), "wrong-size file reads as missing (corrupt)");
        assert_eq!(missing(&[item.clone()]).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fetch_places_the_file_atomically_with_monotonic_progress() {
        let data = blob(256 * 1024);
        let (url, _) = spawn_origin(data.clone(), data.len(), true).await;
        let dir = scratch_dir("atomic");
        let dest = dir.join("w.gguf");
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64 };

        let mut seen: Vec<FetchProgress> = Vec::new();
        fetch_all(std::slice::from_ref(&item), &mut |p| seen.push(p))
            .await
            .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), data, "bytes land intact");
        assert!(!part_path(&dest).exists(), "no .part residue after success");
        assert!(seen.windows(2).all(|w| w[0].received_bytes <= w[1].received_bytes),
            "progress never goes backwards");
        let last = seen.last().unwrap();
        assert_eq!(last.received_bytes, last.total_bytes, "terminal progress is 100%");
        assert_eq!(last.file, "w.gguf");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_shorter_part_file_resumes_with_a_range_request() {
        let data = blob(96 * 1024);
        let (url, ranges) = spawn_origin(data.clone(), data.len(), true).await;
        let dir = scratch_dir("resume");
        let dest = dir.join("w.gguf");
        // A previous attempt left the first 10 000 bytes.
        std::fs::write(part_path(&dest), &data[..10_000]).unwrap();
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64 };

        fetch_all(std::slice::from_ref(&item), &mut |_| {}).await.unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), data, "resumed file is byte-identical");
        assert_eq!(ranges.lock().unwrap().as_slice(), &[Some(10_000)],
            "exactly one request, resuming at the .part length");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_range_ignoring_origin_falls_back_to_a_clean_full_download() {
        let data = blob(64 * 1024);
        let (url, _) = spawn_origin(data.clone(), data.len(), false).await;
        let dir = scratch_dir("norange");
        let dest = dir.join("w.gguf");
        // Poisoned partial content: if the 200 body were APPENDED here, the
        // result would be corrupt at the right size. Truncate-on-200 protects it.
        std::fs::write(part_path(&dest), vec![0xFF; 10_000]).unwrap();
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64 };

        fetch_all(std::slice::from_ref(&item), &mut |_| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_oversized_stale_part_is_discarded_and_refetched() {
        let data = blob(32 * 1024);
        let (url, ranges) = spawn_origin(data.clone(), data.len(), true).await;
        let dir = scratch_dir("stale");
        let dest = dir.join("w.gguf");
        std::fs::write(part_path(&dest), vec![0u8; data.len() + 5]).unwrap();
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64 };

        fetch_all(std::slice::from_ref(&item), &mut |_| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert_eq!(ranges.lock().unwrap().as_slice(), &[None],
            "the oversized .part is thrown away, not resumed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_truncated_upstream_cleans_the_part_and_reports_size_mismatch() {
        let data = blob(48 * 1024);
        // The origin sends 1 KB less than the declared size, with a clean EOF.
        let (url, _) = spawn_origin(data.clone(), data.len() - 1024, true).await;
        let dir = scratch_dir("short");
        let dest = dir.join("w.gguf");
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64 };

        let err = fetch_all(std::slice::from_ref(&item), &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, FetchError::SizeMismatch { expected, actual, .. }
            if expected == data.len() as u64 && actual == data.len() as u64 - 1024));
        assert!(!dest.exists(), "no final file from a truncated body");
        assert!(!part_path(&dest).exists(), "clean retry: the short .part is removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn an_oversized_upstream_never_renames_into_place() {
        let real = blob(16 * 1024);
        // The origin's file grew past the declared size (changed artifact).
        let bigger = blob(16 * 1024 + 512);
        let (url, _) = spawn_origin(bigger.clone(), bigger.len(), true).await;
        let dir = scratch_dir("grown");
        let dest = dir.join("w.gguf");
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: real.len() as u64 };

        let err = fetch_all(std::slice::from_ref(&item), &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, FetchError::SizeMismatch { .. }));
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn already_present_items_are_skipped_and_counted_in_progress() {
        let a = blob(8 * 1024);
        let b = blob(12 * 1024);
        let (url_b, _) = spawn_origin(b.clone(), b.len(), true).await;
        let dir = scratch_dir("partial-install");
        let dest_a = dir.join("a.gguf");
        let dest_b = dir.join("b.gguf");
        std::fs::write(&dest_a, &a).unwrap(); // already installed
        let items = [
            // Dead URL proves the present item is never fetched.
            FetchItem { url: "http://127.0.0.1:9/dead".into(), dest: dest_a, expected_bytes: a.len() as u64 },
            FetchItem { url: url_b, dest: dest_b.clone(), expected_bytes: b.len() as u64 },
        ];
        let mut last: Option<FetchProgress> = None;
        fetch_all(&items, &mut |p| last = Some(p)).await.unwrap();
        let last = last.unwrap();
        assert_eq!(last.total_bytes, (a.len() + b.len()) as u64);
        assert_eq!(last.received_bytes, last.total_bytes,
            "the pre-installed file's bytes count toward overall progress");
        assert_eq!(std::fs::read(&dest_b).unwrap(), b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real ~3.3 GB ingress against the settings-default HF artifacts
    /// (decision #30). `#[ignore]`: network + disk heavy; run deliberately with
    /// `cargo test -p aperture-orchestration model_fetch -- --ignored`.
    #[tokio::test]
    #[ignore = "downloads ~3.3 GB from Hugging Face; run on demand"]
    async fn real_hf_download_matches_the_declared_sizes() {
        let dir = scratch_dir("real-hf");
        let items = [
            FetchItem {
                url: "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf".into(),
                dest: dir.join("qwen2.5-vl-3b-q4_k_m.gguf"),
                expected_bytes: 1_929_901_056,
            },
            FetchItem {
                url: "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf".into(),
                dest: dir.join("qwen2.5-vl-3b-mmproj-f16.gguf"),
                expected_bytes: 1_338_428_128,
            },
        ];
        fetch_all(&items, &mut |_| {}).await.unwrap();
        assert!(items.iter().all(is_present));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
