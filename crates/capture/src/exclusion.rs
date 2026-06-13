//! The exclusion list — data minimization at the earliest gate (doc 05 §4, doc 13 §4).
//!
//! Exclusion stops collection **before** any frame is pulled or OCR runs (doc 05
//! §4). A match by **process**, **window-class**, or **title-regex** yields a
//! metadata-only [`aperture_contracts::Event`] flagged
//! [`redaction_flags::EXCLUDED`] (doc 13 §4); such events **can never appear in
//! any payload** (doc 13 §2/§4). Shipped defaults are a curated, user-editable
//! list: password managers and common banking-domain window titles (doc 13 §4
//! [ASSUMPTION]).
//!
//! Private/incognito browser windows are detected via title-suffix heuristics and
//! treated as excluded, additionally flagged [`redaction_flags::PRIVATE_WINDOW`]
//! (doc 13 §4) [VERIFY reliability per browser].
//!
//! The list lives inside the encrypted DB alongside settings (doc 13 §6); this
//! module holds the matching logic and the compiled in-memory form.

// TODO(M1): exclusion matching lands in M1; the editable-list UI + persistence is M9.

use aperture_contracts::event::redaction_flags;

/// One exclusion rule (doc 05 §4, doc 13 §4). Any populated field that matches
/// excludes the context; an empty field is "don't care".
#[derive(Debug, Clone, Default)]
pub struct ExclusionRule {
    /// Process image name, case-insensitive exact match (e.g. `"1password.exe"`).
    pub process: Option<String>,
    /// Win32 window class, exact match (e.g. a known banking-app shell class).
    pub window_class: Option<String>,
    /// Regex over the window title (e.g. a bank's domain in the tab title).
    /// Stored as the source pattern; compiled into [`ExclusionList`].
    pub title_regex: Option<String>,
    /// Human-readable label surfaced in the redaction summary (doc 13 §5), e.g.
    /// `"1Password"` → preview shows `window_excluded: 1Password`.
    pub label: String,
}

/// The result of an exclusion check (doc 05 §4, doc 13 §4). Carries the
/// redaction-flag bits to OR into the event and the matched rule label for the
/// preview's redaction summary (doc 13 §5).
#[derive(Debug, Clone)]
pub enum ExclusionVerdict {
    /// Not excluded — capture proceeds normally.
    Allowed,
    /// Excluded by a rule. The event becomes metadata-only with these flags set
    /// (at minimum [`redaction_flags::EXCLUDED`]).
    Excluded { flags: u32, label: String },
}

/// The compiled, in-memory exclusion list (doc 05 §4). Built from
/// [`ExclusionRule`]s loaded from the encrypted settings store (doc 13 §6).
/// Cloned cheaply (shared) into the sampler and the normalizer so both can gate.
#[derive(Clone, Default)]
pub struct ExclusionList {
    // rules: std::sync::Arc<Vec<CompiledRule>>,   // title_regex pre-compiled.
}

impl ExclusionList {
    /// Compile a set of rules into a matchable list (doc 05 §4). Pre-compiles each
    /// `title_regex`; an invalid pattern is dropped with a warning (fail-open on a
    /// single bad rule, never fail-closed for the whole list).
    pub fn compile(_rules: Vec<ExclusionRule>) -> Self {
        // TODO(M1): pre-compile title regexes; build the Arc<Vec<CompiledRule>>.
        // [VERIFY] `regex` crate add (pinned, not a workspace dep).
        todo!("M1: compile exclusion rules")
    }

    /// The shipped defaults: password managers + common banking-domain titles
    /// (doc 13 §4 [ASSUMPTION], curated + user-editable).
    pub fn shipped_defaults() -> Self {
        // TODO(M1): seed default rules (1Password, Bitwarden, KeePass, etc.).
        //   M9: merge user edits from the encrypted settings store (doc 13 §6/§7).
        todo!("M1: shipped default exclusion list")
    }

    /// The core predicate (doc 05 §4, doc 13 §4): is this context excluded?
    /// Matched against process / window-class / title in that order. Returns the
    /// verdict carrying the flag bits to set (incl. `EXCLUDED`, and
    /// `PRIVATE_WINDOW` for incognito windows).
    pub fn is_excluded(
        &self,
        _process: Option<&str>,
        _window_class: Option<&str>,
        _title: Option<&str>,
    ) -> ExclusionVerdict {
        // TODO(M1):
        //   - if any rule matches process/class/title:
        //       flags = redaction_flags::EXCLUDED;
        //       if is_private_window(title): flags |= redaction_flags::PRIVATE_WINDOW;
        //       return Excluded { flags, label }
        //   - else if is_private_window(title): treat as excluded (doc 13 §4)
        //   - else Allowed
        let _ = (redaction_flags::EXCLUDED, redaction_flags::PRIVATE_WINDOW);
        todo!("M1: process/class/title exclusion match")
    }
}

/// Heuristic for a private/incognito browser window via title-suffix patterns
/// (doc 13 §4). Treated as excluded with the [`redaction_flags::PRIVATE_WINDOW`]
/// bit. [VERIFY reliability per browser — Chrome "(Incognito)", Edge "[InPrivate]",
/// Firefox "(Private Browsing)" suffixes drift across versions/locales (RK4-adjacent).]
pub fn is_private_window(_title: Option<&str>) -> bool {
    // TODO(M1): match localized incognito/in-private/private-browsing suffixes.
    todo!("M1: private/incognito window detection")
}
