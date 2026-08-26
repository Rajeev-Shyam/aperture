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
//! byte count against the settings-declared size AND the whole-file SHA-256
//! against the settings-declared digest (SDLC review 2026-08-19 finding 5 —
//! a resumed `.part` concatenated with a re-uploaded upstream file is a
//! valid-length, internally inconsistent GGUF that size alone would accept),
//! then rename into place. `SidecarConfig` resolves to these same destination
//! paths even while the files are absent (`main::sidecar_config`), so the very
//! next VLM spawn picks the weights up — no restart.

use std::path::{Path, PathBuf};

/// One weight artifact to fetch: where from, where to, how many bytes the
/// finished file MUST be (both the "already installed?" check and the
/// post-download verification key on this size), and — when the settings
/// declare one — the lowercase-hex SHA-256 the finished bytes MUST hash to
/// (verified once, at download time; see [`is_present`]).
#[derive(Debug, Clone)]
pub struct FetchItem {
    pub url: String,
    pub dest: PathBuf,
    pub expected_bytes: u64,
    /// Lowercase hex SHA-256 of the whole file; `None` = size-only
    /// verification (a settings block that predates finding 5).
    pub sha256: Option<String>,
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
    /// Right size, wrong content: the whole-file SHA-256 disagrees with the
    /// settings-declared digest (finding 5 — a resume stitched across a
    /// re-uploaded upstream, a hostile mirror, or a tampered range response).
    /// The `.part` is deleted so a retry never resumes into the same bad
    /// bytes; the file is NEVER renamed into place.
    #[error("{file}: sha256 {actual} does not match the expected {expected} — download discarded, retry the download")]
    HashMismatch { file: String, expected: String, actual: String },
}

/// Is this artifact already installed? Present means "exists AND is exactly
/// the expected size" — a wrong-size file is a truncated/corrupt download and
/// must read as missing, or the sidecar would spawn against garbage.
///
/// Deliberately size-only: this runs on every Dashboard status poll and before
/// every download click, and re-hashing ~3.3 GB each time is not acceptable.
/// Content is verified once, at download time, by [`fetch_all`] (the
/// `sha256` check before the rename) — a file that reached its final name
/// through this module has already passed it.
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
/// expected size (and, where declared, its expected sha256); any `Err` leaves
/// either a resumable `.part` (transport error) or a clean slate (size or
/// hash mismatch) — never a wrong-size or wrong-content final file.
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

/// Lowercase hex of a finished digest (same shape as
/// `aperture_privacy::audit_log::sha256_hex`, which this crate cannot depend on).
fn hex_lower(digest: &[u8]) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Run the first `len` bytes of `part` (the resume prefix) through `hasher`,
/// so the digest checked at the end covers the whole stitched file and not
/// only the newly received range (finding 5). A prefix shorter than its
/// recorded length means the `.part` changed underneath us — an I/O error,
/// not something to paper over.
async fn hash_prefix(part: &Path, len: u64, hasher: &mut sha2::Sha256) -> Result<(), FetchError> {
    use sha2::Digest;
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(part)
        .await
        .map_err(|e| FetchError::Io(e.to_string()))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut remaining = len;
    while remaining > 0 {
        let want = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
        let n = file
            .read(&mut buf[..want])
            .await
            .map_err(|e| FetchError::Io(e.to_string()))?;
        if n == 0 {
            return Err(FetchError::Io(format!(
                "{}: resume prefix is shorter than its recorded {len} bytes",
                part.display()
            )));
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(())
}

/// Download one artifact to `<dest>.part`, resume-aware, verify (byte count,
/// then whole-file sha256 when the item declares one), rename.
async fn fetch_one<F: FnMut(FetchProgress)>(
    client: &reqwest::Client,
    item: &FetchItem,
    file_index: usize,
    file_count: usize,
    done_bytes: u64,
    total_bytes: u64,
    on_progress: &mut F,
) -> Result<(), FetchError> {
    use sha2::{Digest, Sha256};
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

    // The digest must cover the WHOLE file, so a resume first feeds the bytes
    // already on disk through the hasher. Done before the request goes out: a
    // multi-GB prefix read must never stall an open response body. Wasted
    // only if the origin then ignores the Range (a 200 restarts the hasher).
    let mut hasher = Sha256::new();
    if offset > 0 && item.sha256.is_some() {
        hash_prefix(&part, offset, &mut hasher).await?;
    }

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
        hasher = Sha256::new();
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
        hasher.update(&chunk);
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

    // Right size is necessary, not sufficient (finding 5): the whole-file
    // digest — resume prefix included — must match the settings-declared one.
    // A mismatch is never resumable: discard the `.part` for a clean retry.
    if let Some(expected) = item.sha256.as_deref() {
        let actual = hex_lower(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(FetchError::HashMismatch {
                file: file_name,
                expected: expected.to_string(),
                actual,
            });
        }
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
    tracing::info!(
        file = %file_name,
        bytes = written,
        sha256_verified = item.sha256.is_some(),
        "VLM artifact installed (decision #30)"
    );
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
        let item = FetchItem { url: String::new(), dest: dest.clone(), expected_bytes: 8, sha256: None };
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
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64, sha256: None };

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
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64, sha256: None };

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
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64, sha256: None };

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
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64, sha256: None };

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
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: data.len() as u64, sha256: None };

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
        let item = FetchItem { url, dest: dest.clone(), expected_bytes: real.len() as u64, sha256: None };

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
            FetchItem { url: "http://127.0.0.1:9/dead".into(), dest: dest_a, expected_bytes: a.len() as u64, sha256: None },
            FetchItem { url: url_b, dest: dest_b.clone(), expected_bytes: b.len() as u64, sha256: None },
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

    fn sha256_of(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex_lower(&Sha256::digest(data))
    }

    /// A loopback origin that sends `Content-Length: <full>` but only the first
    /// `send_len` bytes, then drops the socket — a mid-stream transport failure
    /// (the axum fake's truncation is a clean EOF, which is a different case:
    /// that one gets discarded as a SizeMismatch, this one must stay resumable).
    async fn spawn_dropping_origin(data: Vec<u8>, send_len: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/weights.gguf", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = vec![0u8; 4096];
            let _ = sock.read(&mut req).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&data[..send_len]).await.unwrap();
            sock.flush().await.unwrap();
            drop(sock);
        });
        url
    }

    /// Finding 5: right size is not enough — a served body whose digest differs
    /// from the declared one is rejected, never renamed, and leaves no `.part`
    /// (a retry must not resume into the same bad bytes).
    #[tokio::test]
    async fn a_wrong_hash_is_rejected_and_leaves_no_part() {
        let data = blob(40 * 1024);
        let (url, _) = spawn_origin(data.clone(), data.len(), true).await;
        let dir = scratch_dir("badhash");
        let dest = dir.join("w.gguf");
        let mut wrong = blob(40 * 1024);
        wrong[100] ^= 0x01;
        let expected = sha256_of(&wrong);
        let item = FetchItem {
            url,
            dest: dest.clone(),
            expected_bytes: data.len() as u64,
            sha256: Some(expected.clone()),
        };

        let err = fetch_all(std::slice::from_ref(&item), &mut |_| {}).await.unwrap_err();
        match err {
            FetchError::HashMismatch { file, expected: e, actual } => {
                assert_eq!(file, "w.gguf");
                assert_eq!(e, expected);
                assert_eq!(actual, sha256_of(&data), "the reported digest is the served bytes'");
            }
            other => panic!("expected HashMismatch, got {other:?}"),
        }
        assert!(!dest.exists(), "never renamed into place");
        assert!(!part_path(&dest).exists(), "no resumable .part from a hash mismatch");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_correct_hash_passes_and_installs() {
        let data = blob(40 * 1024);
        let (url, _) = spawn_origin(data.clone(), data.len(), true).await;
        let dir = scratch_dir("goodhash");
        let dest = dir.join("w.gguf");
        let item = FetchItem {
            url,
            dest: dest.clone(),
            expected_bytes: data.len() as u64,
            // Uppercase on purpose: the comparison is case-insensitive.
            sha256: Some(sha256_of(&data).to_ascii_uppercase()),
        };
        fetch_all(std::slice::from_ref(&item), &mut |_| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), data);
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The finding-5 scenario end to end: request 1 dies mid-stream (resumable
    /// `.part` kept), request 2 resumes via Range — and the digest checked at
    /// the end covers the WHOLE stitched file, so a resume onto a consistent
    /// upstream passes while a resume onto a re-uploaded one is rejected.
    #[tokio::test]
    async fn a_resume_across_two_requests_verifies_the_whole_file_hash() {
        let data = blob(96 * 1024);
        let dir = scratch_dir("resume-hash");
        let dest = dir.join("w.gguf");
        let expected = sha256_of(&data);

        // Request 1: transport drop after 20 000 bytes.
        let url1 = spawn_dropping_origin(data.clone(), 20_000).await;
        let first = FetchItem {
            url: url1,
            dest: dest.clone(),
            expected_bytes: data.len() as u64,
            sha256: Some(expected.clone()),
        };
        let err = fetch_all(std::slice::from_ref(&first), &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, FetchError::Http(_)), "transport drop, got {err:?}");
        assert_eq!(std::fs::metadata(part_path(&dest)).unwrap().len(), 20_000,
            "the partial bytes stay on disk for the resume");

        // Request 2a: the upstream was re-uploaded (same size, different
        // bytes) — the stitched file has the right length, wrong content.
        let mut reuploaded = data.clone();
        reuploaded[50_000] ^= 0x01;
        let (url2, ranges) = spawn_origin(reuploaded, data.len(), true).await;
        let second = FetchItem { url: url2, ..first.clone() };
        let err = fetch_all(std::slice::from_ref(&second), &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, FetchError::HashMismatch { .. }), "got {err:?}");
        assert_eq!(ranges.lock().unwrap().as_slice(), &[Some(20_000)], "it did resume");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists(), "a stitched mismatch is not resumable");

        // Request 2b: the same drop-then-resume against a consistent upstream
        // — the prefix on disk + the resumed range hash to the declared digest.
        let url1 = spawn_dropping_origin(data.clone(), 20_000).await;
        let first = FetchItem { url: url1, ..first.clone() };
        let err = fetch_all(std::slice::from_ref(&first), &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, FetchError::Http(_)), "got {err:?}");
        let (url3, ranges) = spawn_origin(data.clone(), data.len(), true).await;
        let third = FetchItem { url: url3, ..first.clone() };
        fetch_all(std::slice::from_ref(&third), &mut |_| {}).await.unwrap();
        assert_eq!(ranges.lock().unwrap().as_slice(), &[Some(20_000)], "it did resume");
        assert_eq!(std::fs::read(&dest).unwrap(), data, "stitched file is byte-identical");
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A poisoned prefix (right length, wrong bytes) must be caught even when
    /// every byte the NETWORK delivered was correct — proof the resume prefix
    /// is hashed, not just the new range.
    #[tokio::test]
    async fn a_poisoned_resume_prefix_fails_the_whole_file_hash() {
        let data = blob(64 * 1024);
        let (url, ranges) = spawn_origin(data.clone(), data.len(), true).await;
        let dir = scratch_dir("poisoned-prefix");
        let dest = dir.join("w.gguf");
        std::fs::write(part_path(&dest), vec![0xFF; 10_000]).unwrap();
        let item = FetchItem {
            url,
            dest: dest.clone(),
            expected_bytes: data.len() as u64,
            sha256: Some(sha256_of(&data)),
        };
        let err = fetch_all(std::slice::from_ref(&item), &mut |_| {}).await.unwrap_err();
        assert!(matches!(err, FetchError::HashMismatch { .. }), "got {err:?}");
        assert_eq!(ranges.lock().unwrap().as_slice(), &[Some(10_000)]);
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real ~3.3 GB ingress against the settings-default HF artifacts
    /// (decision #30), pinned to the repo revision + sha256 that
    /// `config/settings.default.json` declares (finding 5). `#[ignore]`: network
    /// + disk heavy; run deliberately with
    /// `cargo test -p aperture-orchestration model_fetch -- --ignored`.
    #[tokio::test]
    #[ignore = "downloads ~3.3 GB from Hugging Face; run on demand"]
    async fn real_hf_download_matches_the_declared_sizes() {
        let dir = scratch_dir("real-hf");
        let items = [
            FetchItem {
                url: "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/5037fcf163dd95d1e41d1974465f0898ed108ca2/Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf".into(),
                dest: dir.join("qwen2.5-vl-3b-q4_k_m.gguf"),
                expected_bytes: 1_929_901_056,
                sha256: Some("d02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12".into()),
            },
            FetchItem {
                url: "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/5037fcf163dd95d1e41d1974465f0898ed108ca2/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf".into(),
                dest: dir.join("qwen2.5-vl-3b-mmproj-f16.gguf"),
                expected_bytes: 1_338_428_128,
                sha256: Some("b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e".into()),
            },
        ];
        fetch_all(&items, &mut |_| {}).await.unwrap();
        assert!(items.iter().all(is_present));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
