//! Exclusion lists & the private/incognito heuristic (doc 13 §4).
//!
//! Data minimization at the source: excluded contexts are stopped at the
//! earliest gate (doc 05 §4). A foreground app/window matching the exclusion
//! list yields a **metadata-only** event flagged
//! [`redaction_flags::EXCLUDED`] — no frame, no OCR — and can never appear in
//! any payload. Private/incognito browser windows are detected via title-suffix
//! heuristics and treated as excluded, flagged
//! [`redaction_flags::PRIVATE_WINDOW`] (doc 13 §4) [VERIFY reliability per browser].
//!
//! **Defaults ship EMPTY** (ADR-029/Q15): sensitive-app protection comes from
//! onboarding's **detect-and-suggest** (scan installed password managers /
//! banking apps locally, *suggest* exclusions the user confirms — never
//! auto-excluded; ADR-040) plus the one-click "exclude this domain/app"
//! affordances. Match kinds include `url_pattern` (ADR-040) for
//! extension-sourced URLs — which traverse this same pipeline (FIX 2.2).
//! The list itself lives in the encrypted settings table (doc 13 §6).
//!
//! INVARIANT (2): this gate is purely local; it removes data from collection, it
//! never transmits anything.

use aperture_contracts::event::redaction_flags;

/// One exclusion rule. A context is excluded if **any** populated matcher hits
/// (doc 05 §4 / doc 13 §4).
#[derive(Debug, Clone)]
pub struct ExclusionRule {
    /// Process image name, e.g. `"1Password.exe"` (case-insensitive).
    pub process: Option<String>,
    /// Win32 window class name.
    pub window_class: Option<String>,
    /// Regex over the window title.
    pub title_regex: Option<String>,
    /// `false` for a user-disabled rule; shipped defaults start enabled.
    pub enabled: bool,
}

/// A foreground context to test against the exclusion list. Sourced from the
/// capture layer's focus tracker (doc 05 §3).
#[derive(Debug, Clone)]
pub struct ForegroundContext<'a> {
    pub process: Option<&'a str>,
    pub window_class: Option<&'a str>,
    pub window_title: Option<&'a str>,
}

/// The verdict for a foreground context. Maps directly onto the event's
/// `redaction_flags` bitfield (doc 13 §4, contract `event.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionVerdict {
    /// Capture normally.
    Allowed,
    /// Matched the exclusion list -> `EXCLUDED`, metadata-only.
    Excluded,
    /// Private/incognito window -> `PRIVATE_WINDOW`, treated as excluded.
    PrivateWindow,
}

impl ExclusionVerdict {
    /// The `redaction_flags` bits this verdict implies (`0` for `Allowed`).
    /// A private window is also excluded, so it sets both bits.
    pub fn flags(self) -> u32 {
        match self {
            ExclusionVerdict::Allowed => 0,
            ExclusionVerdict::Excluded => redaction_flags::EXCLUDED,
            ExclusionVerdict::PrivateWindow => {
                redaction_flags::EXCLUDED | redaction_flags::PRIVATE_WINDOW
            }
        }
    }

    /// Whether capture must be suppressed (no frame/OCR) for this verdict.
    pub fn is_excluded(self) -> bool {
        !matches!(self, ExclusionVerdict::Allowed)
    }
}

/// Holds the active exclusion list and the compiled title regexes.
pub struct ExclusionManager {
    // TODO(M9): Vec<ExclusionRule> + compiled title regexes; loaded from the
    // encrypted settings table (doc 13 §6).
}

impl ExclusionManager {
    /// Load the user's exclusion list merged with shipped defaults
    /// ([`default_rules`]).
    pub fn new(_rules: Vec<ExclusionRule>) -> Self {
        // TODO(M9): compile title_regex of each enabled rule; store.
        todo!("M9: build exclusion manager from rules (doc 13 §4)")
    }

    /// Classify a foreground context. Order: explicit exclusion rule first, then
    /// the private-window heuristic (doc 13 §4).
    pub fn classify(&self, _ctx: &ForegroundContext<'_>) -> ExclusionVerdict {
        // TODO(M9):
        //   if any enabled rule matches (process | window_class | title_regex)
        //       -> Excluded
        //   else if is_private_window(title) -> PrivateWindow
        //   else -> Allowed
        todo!("M9: classify foreground context (doc 13 §4)")
    }

    /// Add an exclusion rule from a bubble's "exclude this app" affordance — the
    /// one-click recovery for an exclusion-list gap (doc 13 §9).
    pub fn add_rule(&mut self, _rule: ExclusionRule) {
        // TODO(M9): push + recompile + persist to encrypted settings.
        todo!("M9: add exclusion rule (doc 13 §9)")
    }
}

/// Private/incognito detection by window-title suffix heuristic (doc 13 §4).
/// Examples of suffixes that indicate a private context across major browsers
/// [VERIFY reliability per browser]:
///   - Chrome/Edge/Brave: `"… - Incognito"` / `"… - InPrivate"`
///   - Firefox: `"… (Private Browsing)"` / `"… — Mozilla Firefox Private Browsing"`
pub fn is_private_window(_window_title: &str) -> bool {
    // TODO(M9): match against PRIVATE_TITLE_SUFFIXES (case-insensitive).
    // [VERIFY] suffix strings against shipping browser builds at M9.
    todo!("M9: private-window title heuristic (doc 13 §4)")
}

/// Shipped default exclusion rules: **EMPTY** (ADR-029/Q15). The curated
/// password-manager/banking list became onboarding *suggestions* the user
/// confirms (detect-and-suggest, ADR-040/M9) — never silent auto-exclusions.
pub fn default_rules() -> Vec<ExclusionRule> {
    Vec::new()
}
