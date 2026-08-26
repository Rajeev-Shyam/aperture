//! VLM weight install state + settings plumbing (decision #30, doc 06 §3).
//!
//! The ~3.3 GB Qwen2.5-VL weights cannot ship in the installer (NSIS 2 GB
//! cap), so fresh machines used to degrade to OCR-only with no signal. This
//! module gives the shell the pieces to be honest about it: the resolved
//! destination paths (the SAME paths `main::sidecar_config` hands the
//! spawner, so a finished download is live on the next VLM spawn — no
//! restart), and the settings-declared download spec.
//!
//! The download itself runs in `aperture_orchestration::model_fetch` — the
//! lint-emitters SANCTIONED crate — and ONLY from the user's explicit
//! Dashboard click (`commands::vlm_download`). The shell opens no sockets
//! (doc 13 §2); it only forwards progress to the WebView.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use aperture_orchestration::model_fetch::FetchItem;

/// The weight file names `SidecarConfig` spawns with (doc 04 §3 L1). The
/// download destination and the spawn path must agree on these by construction.
pub const VLM_MODEL_FILE: &str = "qwen2.5-vl-3b-q4_k_m.gguf";
pub const VLM_MMPROJ_FILE: &str = "qwen2.5-vl-3b-mmproj-f16.gguf";

// Fallback download spec, mirroring `config/settings.default.json`'s
// `loadout.vlm_download` byte-for-byte. Needed because the settings seed runs
// on FIRST run only — an upgraded install never re-seeds, so its settings rows
// predate this key (the additive backfill in `main` adds missing leaves such
// as `sha256`, never rewrites a stored `url`). NG8 still holds: the settings
// value, when present, always wins; these are the same values the seed would
// have written. SDLC review 2026-08-19 finding 5: URLs pinned to HF revision
// 5037fcf163dd95d1e41d1974465f0898ed108ca2 (not the moving `main`) + the
// whole-file sha256. Size + sha256 verified against the ggml-org HF repo and
// the dev machine's files 2026-08-22.
const DEFAULT_MODEL_URL: &str =
    "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/5037fcf163dd95d1e41d1974465f0898ed108ca2/Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf";
const DEFAULT_MODEL_BYTES: u64 = 1_929_901_056;
const DEFAULT_MODEL_SHA256: &str = "d02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12";
const DEFAULT_MMPROJ_URL: &str =
    "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/5037fcf163dd95d1e41d1974465f0898ed108ca2/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf";
const DEFAULT_MMPROJ_BYTES: u64 = 1_338_428_128;
const DEFAULT_MMPROJ_SHA256: &str = "b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e";

/// Shared VLM-install state (in [`crate::app_state::AppState`]). The dest
/// paths are the resolved `SidecarConfig` weight paths — one source of truth
/// for "where the spawner looks" and "where the download lands".
pub struct VlmFetchState {
    pub model_dest: PathBuf,
    pub mmproj_dest: PathBuf,
    /// True while a download task runs; guards double-starts. Cleared by the
    /// task on ANY terminal state (done or error).
    pub in_flight: AtomicBool,
}

impl VlmFetchState {
    pub fn new(model_dest: PathBuf, mmproj_dest: PathBuf) -> Self {
        Self {
            model_dest,
            mmproj_dest,
            in_flight: AtomicBool::new(false),
        }
    }
}

/// Build the two-artifact fetch spec from the `loadout.vlm_download` settings
/// block (URLs + sizes + sha256 are settings, never code — NG8), with the
/// seed-mirroring defaults for any missing/invalid key so a typo can never
/// brick the download.
///
/// `sha256` (finding 5) follows the URL rather than falling back on its own:
/// a present key decides (`""` is the explicit opt-out for a custom mirror of
/// a different file ⇒ size-only, as before finding 5); an absent key takes the
/// default digest only when the URL also fell back to the default — the
/// default hash is right for the default artifact and nothing else.
pub fn spec_from_settings(loadout: &serde_json::Value, state: &VlmFetchState) -> Vec<FetchItem> {
    let dl = loadout.get("vlm_download");
    vec![
        item(
            dl.and_then(|d| d.get("model")),
            DEFAULT_MODEL_URL,
            DEFAULT_MODEL_BYTES,
            DEFAULT_MODEL_SHA256,
            &state.model_dest,
        ),
        item(
            dl.and_then(|d| d.get("mmproj")),
            DEFAULT_MMPROJ_URL,
            DEFAULT_MMPROJ_BYTES,
            DEFAULT_MMPROJ_SHA256,
            &state.mmproj_dest,
        ),
    ]
}

fn item(
    section: Option<&serde_json::Value>,
    default_url: &str,
    default_bytes: u64,
    default_sha256: &str,
    dest: &Path,
) -> FetchItem {
    let url = section
        .and_then(|s| s.get("url"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_url)
        .to_string();
    let expected_bytes = section
        .and_then(|s| s.get("bytes"))
        .and_then(serde_json::Value::as_u64)
        .filter(|b| *b > 0)
        .unwrap_or(default_bytes);
    let sha256 = match section.and_then(|s| s.get("sha256")) {
        Some(v) => v
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase),
        None => (url == default_url).then(|| default_sha256.to_string()),
    };
    FetchItem {
        url,
        dest: dest.to_path_buf(),
        expected_bytes,
        sha256,
    }
}

/// The `loadout` settings section (missing/unparseable ⇒ `{}` — the defaults
/// above take over, same contract as `main::read_settings_section`).
pub(crate) fn loadout_section(db: &aperture_db::Db) -> serde_json::Value {
    db.get_setting("loadout")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> VlmFetchState {
        VlmFetchState::new(
            PathBuf::from("models").join(VLM_MODEL_FILE),
            PathBuf::from("models").join(VLM_MMPROJ_FILE),
        )
    }

    /// An upgraded install (settings seeded before `vlm_download` existed) must
    /// still get a working spec — the code defaults mirror the seed.
    #[test]
    fn missing_settings_block_falls_back_to_the_seed_mirroring_defaults() {
        let spec = spec_from_settings(&serde_json::json!({}), &state());
        assert_eq!(spec.len(), 2);
        assert_eq!(spec[0].url, DEFAULT_MODEL_URL);
        assert_eq!(spec[0].expected_bytes, DEFAULT_MODEL_BYTES);
        assert_eq!(spec[0].sha256.as_deref(), Some(DEFAULT_MODEL_SHA256));
        assert_eq!(spec[0].dest, PathBuf::from("models").join(VLM_MODEL_FILE));
        assert_eq!(spec[1].url, DEFAULT_MMPROJ_URL);
        assert_eq!(spec[1].expected_bytes, DEFAULT_MMPROJ_BYTES);
        assert_eq!(spec[1].sha256.as_deref(), Some(DEFAULT_MMPROJ_SHA256));
        assert_eq!(spec[1].dest, PathBuf::from("models").join(VLM_MMPROJ_FILE));
    }

    /// The code defaults must be the seed, byte for byte (finding 5: pinned
    /// revision + sha256 included) — the shipped JSON is the source of truth.
    #[test]
    fn defaults_mirror_the_shipped_seed() {
        let seed: serde_json::Value =
            serde_json::from_str(include_str!("../../config/settings.default.json")).unwrap();
        let dl = &seed["loadout"]["vlm_download"];
        assert_eq!(dl["model"]["url"], DEFAULT_MODEL_URL);
        assert_eq!(dl["model"]["bytes"], DEFAULT_MODEL_BYTES);
        assert_eq!(dl["model"]["sha256"], DEFAULT_MODEL_SHA256);
        assert_eq!(dl["mmproj"]["url"], DEFAULT_MMPROJ_URL);
        assert_eq!(dl["mmproj"]["bytes"], DEFAULT_MMPROJ_BYTES);
        assert_eq!(dl["mmproj"]["sha256"], DEFAULT_MMPROJ_SHA256);
        for url in [DEFAULT_MODEL_URL, DEFAULT_MMPROJ_URL] {
            assert!(!url.contains("/resolve/main/"), "revision must be pinned, not `main`: {url}");
        }
        for sha in [DEFAULT_MODEL_SHA256, DEFAULT_MMPROJ_SHA256] {
            assert_eq!(sha.len(), 64);
            assert!(sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "lowercase hex: {sha}");
        }
    }

    /// NG8: a settings-declared URL/size/sha256 always wins over the code default.
    #[test]
    fn settings_values_override_the_defaults() {
        let loadout = serde_json::json!({
            "vlm_download": {
                "model": { "url": "https://mirror.example/model.gguf", "bytes": 42, "sha256": "ABCD" },
                "mmproj": { "url": "https://mirror.example/mmproj.gguf", "bytes": 7, "sha256": " 0123 " }
            }
        });
        let spec = spec_from_settings(&loadout, &state());
        assert_eq!(spec[0].url, "https://mirror.example/model.gguf");
        assert_eq!(spec[0].expected_bytes, 42);
        assert_eq!(spec[0].sha256.as_deref(), Some("abcd"), "normalized to lowercase hex");
        assert_eq!(spec[1].url, "https://mirror.example/mmproj.gguf");
        assert_eq!(spec[1].expected_bytes, 7);
        assert_eq!(spec[1].sha256.as_deref(), Some("0123"), "trimmed");
    }

    /// A typo'd block (empty URL, zero/negative size) must never produce an
    /// unfetchable spec — each bad field falls back independently.
    #[test]
    fn invalid_fields_fall_back_independently() {
        let loadout = serde_json::json!({
            "vlm_download": {
                "model": { "url": "", "bytes": 0 },
                "mmproj": { "url": "https://mirror.example/mmproj.gguf", "bytes": -3 }
            }
        });
        let spec = spec_from_settings(&loadout, &state());
        assert_eq!(spec[0].url, DEFAULT_MODEL_URL, "empty url ignored");
        assert_eq!(spec[0].expected_bytes, DEFAULT_MODEL_BYTES, "zero size ignored");
        assert_eq!(spec[0].sha256.as_deref(), Some(DEFAULT_MODEL_SHA256),
            "url fell back to the default, so its digest applies");
        assert_eq!(spec[1].url, "https://mirror.example/mmproj.gguf");
        assert_eq!(spec[1].expected_bytes, DEFAULT_MMPROJ_BYTES, "negative size ignored");
        assert_eq!(spec[1].sha256, None,
            "a custom url with no sha256 key is size-only — the default digest is not its digest");
    }

    /// `sha256: ""` is the explicit opt-out (a custom mirror of a different
    /// file); a non-string value is ignored the same way. Neither silently
    /// substitutes the default digest.
    #[test]
    fn an_empty_or_invalid_sha256_means_size_only() {
        let loadout = serde_json::json!({
            "vlm_download": {
                "model": { "sha256": "" },
                "mmproj": { "sha256": 12345 }
            }
        });
        let spec = spec_from_settings(&loadout, &state());
        assert_eq!(spec[0].url, DEFAULT_MODEL_URL);
        assert_eq!(spec[0].sha256, None, "explicit empty = opt-out, even on the default url");
        assert_eq!(spec[1].url, DEFAULT_MMPROJ_URL);
        assert_eq!(spec[1].sha256, None, "non-string ignored");
    }
}
