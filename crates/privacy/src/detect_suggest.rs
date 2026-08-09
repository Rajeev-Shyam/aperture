//! Detect-and-suggest onboarding (doc 13 §4, §8; ADR-029/ADR-040).
//!
//! Exclusion defaults ship **EMPTY** (ADR-029/Q15) — the user chose maximum
//! control. The compensating control is this: at first run we scan **locally**
//! for installed password managers / banking-ish apps and *suggest* exclusions
//! the user confirms. Nothing here ever auto-excludes, and nothing here leaves
//! the machine (INVARIANT 2 — no network, this is a filesystem/registry read).
//!
//! Why suggestions and not defaults: a shipped blocklist is both over-broad (it
//! excludes apps the user wanted captured) and under-broad (it misses their
//! actual bank), and it silently decides for them. A confirmed suggestion is
//! honest about who is choosing.
//!
//! The matching half of exclusion lives in `aperture_capture::exclusion`
//! (`ExclusionList`), the persisted half in `aperture_db` (`exclusion_list`
//! table). This module only produces *candidates*.

use serde::{Deserialize, Serialize};

/// One suggested exclusion, rendered as a confirm/skip row in onboarding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedExclusion {
    /// `exclusion_list.match_kind` — `process` for installed-app hits.
    pub match_kind: String,
    /// `exclusion_list.pattern` — e.g. `"1password.exe"`.
    pub pattern: String,
    /// Human-readable name shown in the confirm row, e.g. `"1Password"`.
    pub label: String,
    /// Why we are suggesting it, shown as the row's subtitle. Keeps the flow
    /// honest: the user sees the reason, not just the recommendation.
    pub reason: String,
}

/// A known-sensitive application: its display name and the process image we
/// would exclude. Matched case-insensitively against what is actually installed.
struct KnownApp {
    label: &'static str,
    process: &'static str,
    reason: &'static str,
}

/// The curated catalogue. Deliberately small and mainstream — this is a
/// *suggestion* list, not a security boundary, and every entry the user does not
/// recognise costs trust. Password managers and desktop banking/tax clients are
/// the two categories where accidental capture is most damaging.
const KNOWN_APPS: &[KnownApp] = &[
    KnownApp { label: "1Password", process: "1password.exe", reason: "password manager" },
    KnownApp { label: "Bitwarden", process: "bitwarden.exe", reason: "password manager" },
    KnownApp { label: "KeePass", process: "keepass.exe", reason: "password manager" },
    KnownApp { label: "KeePassXC", process: "keepassxc.exe", reason: "password manager" },
    KnownApp { label: "LastPass", process: "lastpass.exe", reason: "password manager" },
    KnownApp { label: "Dashlane", process: "dashlane.exe", reason: "password manager" },
    KnownApp { label: "NordPass", process: "nordpass.exe", reason: "password manager" },
    KnownApp { label: "Proton Pass", process: "proton pass.exe", reason: "password manager" },
    KnownApp { label: "Windows Security", process: "securityhealthhost.exe", reason: "credential prompts" },
    KnownApp { label: "Remote Desktop", process: "mstsc.exe", reason: "shows other machines' screens" },
    KnownApp { label: "Quicken", process: "qw.exe", reason: "personal finance" },
    KnownApp { label: "GnuCash", process: "gnucash.exe", reason: "personal finance" },
];

/// Build suggestions from a set of installed executable names (lowercased by
/// [`installed_process_names`]). Pure — the scan is injected so this is testable
/// without touching the real machine.
///
/// `already_excluded` are the patterns already in the user's list; those are
/// filtered out so onboarding never re-asks about a rule they already have.
pub fn suggest_from(
    installed: &[String],
    already_excluded: &[String],
) -> Vec<SuggestedExclusion> {
    let installed: std::collections::HashSet<String> =
        installed.iter().map(|s| s.to_ascii_lowercase()).collect();
    let excluded: std::collections::HashSet<String> =
        already_excluded.iter().map(|s| s.to_ascii_lowercase()).collect();

    KNOWN_APPS
        .iter()
        .filter(|app| installed.contains(app.process) && !excluded.contains(app.process))
        .map(|app| SuggestedExclusion {
            match_kind: "process".to_string(),
            pattern: app.process.to_string(),
            label: app.label.to_string(),
            reason: app.reason.to_string(),
        })
        .collect()
}

/// Scan the local filesystem for installed executables (doc 13 §8).
///
/// Walks the standard install roots one level deep for `*.exe` — enough to spot
/// `…\1Password\1Password.exe` without a full-disk crawl (which would be slow
/// and creepy for an onboarding step). Unreadable directories are skipped, never
/// fatal: a failed scan yields *fewer suggestions*, never a blocked first run.
///
/// Returns lowercased file names, the shape [`suggest_from`] expects.
pub fn installed_process_names() -> Vec<String> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(v) = std::env::var_os(var) {
            roots.push(std::path::PathBuf::from(v));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(std::path::PathBuf::from(local).join("Programs"));
    }

    let mut found = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                collect_exe(&path, &mut found);
                continue;
            }
            // One level into each install directory.
            let Ok(inner) = std::fs::read_dir(&path) else { continue };
            for item in inner.flatten() {
                collect_exe(&item.path(), &mut found);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

fn collect_exe(path: &std::path::Path, out: &mut Vec<String>) {
    if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("exe")) != Some(true)
    {
        return;
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        out.push(name.to_ascii_lowercase());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_only_apps_that_are_actually_installed() {
        let installed = vec!["1password.exe".to_string(), "notepad.exe".to_string()];
        let out = suggest_from(&installed, &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pattern, "1password.exe");
        assert_eq!(out[0].label, "1Password");
        assert_eq!(out[0].match_kind, "process");
        assert!(!out[0].reason.is_empty(), "the user is shown WHY");
    }

    #[test]
    fn nothing_is_suggested_when_nothing_sensitive_is_installed() {
        // ADR-029: defaults ship empty and stay empty absent a real hit.
        let out = suggest_from(&["notepad.exe".into(), "code.exe".into()], &[]);
        assert!(out.is_empty());
    }

    #[test]
    fn matching_is_case_insensitive() {
        let out = suggest_from(&["KeePassXC.EXE".into()], &[]);
        assert_eq!(out.len(), 1, "installed names arrive in any case");
        assert_eq!(out[0].pattern, "keepassxc.exe");
    }

    #[test]
    fn already_excluded_apps_are_not_re_suggested() {
        let installed = vec!["1password.exe".to_string(), "bitwarden.exe".to_string()];
        let out = suggest_from(&installed, &["1Password.exe".to_string()]);
        assert_eq!(out.len(), 1, "an existing rule must not be re-asked");
        assert_eq!(out[0].pattern, "bitwarden.exe");
    }

    #[test]
    fn suggestions_are_never_auto_applied() {
        // Guard against a future refactor turning the catalogue into defaults:
        // `ExclusionList::shipped_defaults()` must stay empty (ADR-029/Q15), and
        // this module must only ever RETURN candidates.
        let out = suggest_from(&["1password.exe".into()], &[]);
        assert_eq!(out.len(), 1);
        // The type carries no "enabled" bit — applying is the caller's explicit act.
        let json = serde_json::to_value(&out[0]).unwrap();
        assert!(json.get("enabled").is_none(), "a suggestion cannot carry its own approval");
    }

    #[test]
    fn the_local_scan_never_panics_and_never_egresses() {
        // Smoke test: the scan must survive an arbitrary machine layout. It is a
        // filesystem read only — no network (INVARIANT 2).
        let names = installed_process_names();
        assert!(names.iter().all(|n| n.ends_with(".exe")), "only exe names are collected");
        assert!(names.iter().all(|n| n == &n.to_ascii_lowercase()), "names are normalized");
    }
}
