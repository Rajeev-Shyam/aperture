//! The exclusion list — data minimization at the earliest gate (doc 05 §4, doc 13 §4).
//!
//! Exclusion stops collection **before** any frame is pulled or OCR runs (doc 05
//! §4). A match by **process**, **window-class**, **title-regex**, or
//! **url_pattern** (ADR-040) yields a metadata-only
//! [`aperture_contracts::Event`] flagged [`redaction_flags::EXCLUDED`]
//! (doc 13 §4); such events **can never appear in any payload** (doc 13 §2/§4).
//! **Defaults are a curated durable seed** (owner decision #20, Doc 24,
//! 2026-08-16 — supersedes ADR-029/Q15's empty-default posture): on first
//! launch [`SHIPPED_DEFAULT_RULES`] (password managers + generic banking-style
//! patterns) is seeded ONCE into the durable `exclusion_list` table, guarded by
//! the [`EXCLUSION_DEFAULTS_SEEDED_KEY`] settings flag. Seeded rows are
//! ordinary rows: user-visible, disableable, and permanently deletable in the
//! exclusion manager — a deleted default never resurrects on a later launch
//! ([`defaults_needing_seed`]). The onboarding **detect-and-suggest** flow
//! (suggest-only, never auto-excluded) and the one-click "exclude this
//! domain/app" affordances (ADR-040) still run ON TOP of the seed.
//!
//! Private/incognito browser windows are detected via title-suffix heuristics and
//! treated as excluded, additionally flagged [`redaction_flags::PRIVATE_WINDOW`]
//! (doc 13 §4) [VERIFY reliability per browser].
//!
//! The list lives inside the encrypted DB alongside settings (doc 13 §6); this
//! module holds the matching logic and the compiled in-memory form.

use std::sync::Arc;

use aperture_contracts::event::redaction_flags;

/// One exclusion rule (doc 05 §4, doc 13 §4, ADR-040). Any populated field that
/// matches excludes the context; an empty field is "don't care".
#[derive(Debug, Clone, Default)]
pub struct ExclusionRule {
    /// Process image name, case-insensitive exact match (e.g. `"1password.exe"`).
    pub process: Option<String>,
    /// Win32 window class, exact match (e.g. a known banking-app shell class).
    pub window_class: Option<String>,
    /// Regex over the window title (e.g. a bank's domain in the tab title).
    pub title_regex: Option<String>,
    /// Regex over a captured URL (`url_pattern` kind, ADR-040) — matched against
    /// extension/UIA-sourced URLs, which traverse this same gate (FIX 2.2).
    pub url_pattern: Option<String>,
    /// Human-readable label surfaced in the redaction summary (doc 13 §5), e.g.
    /// `"1Password"` → preview shows `window_excluded: 1Password`.
    pub label: String,
}

/// The result of an exclusion check (doc 05 §4, doc 13 §4). Carries the
/// redaction-flag bits to OR into the event and the matched rule label for the
/// preview's redaction summary (doc 13 §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusionVerdict {
    /// Not excluded — capture proceeds normally.
    Allowed,
    /// Excluded by a rule (or the private-window heuristic). The event becomes
    /// metadata-only with these flags set (at minimum [`redaction_flags::EXCLUDED`]).
    Excluded { flags: u32, label: String },
}

impl ExclusionVerdict {
    /// Whether capture must be suppressed (no frame/OCR/connector capture).
    pub fn is_excluded(&self) -> bool {
        matches!(self, ExclusionVerdict::Excluded { .. })
    }
}

/// A compiled rule: literal matchers lowercased once, regexes pre-compiled.
#[derive(Debug)]
struct CompiledRule {
    process: Option<String>,
    window_class: Option<String>,
    title_regex: Option<regex::Regex>,
    url_pattern: Option<regex::Regex>,
    label: String,
}

/// The compiled, in-memory exclusion list (doc 05 §4). Built from
/// [`ExclusionRule`]s loaded from the encrypted settings store (doc 13 §6).
/// Cloned cheaply (shared) into the sampler and the normalizer so both can gate.
///
/// Interior-mutable since the M9 follow-up: every clone shares ONE swappable
/// rule set, so [`replace`](Self::replace) (the Activity & Privacy view's
/// add/disable path) takes effect on the next frame/event in every holder —
/// no restart. Readers take a snapshot `Arc` per check; the write lock is held
/// only for the pointer swap.
#[derive(Clone, Default)]
pub struct ExclusionList {
    rules: Arc<std::sync::RwLock<Arc<Vec<CompiledRule>>>>,
}

impl std::fmt::Debug for ExclusionList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ExclusionList({} rules)", self.len())
    }
}

/// Compile rules, dropping (with a warning) any matcher whose regex is invalid
/// (fail-open on a single bad rule, never fail-closed for the whole list).
fn compile_rules(rules: Vec<ExclusionRule>) -> Vec<CompiledRule> {
    rules
        .into_iter()
        .map(|r| {
            let compile = |src: Option<&String>, kind: &str| -> Option<regex::Regex> {
                let src = src?;
                match regex::RegexBuilder::new(src).case_insensitive(true).build() {
                    Ok(re) => Some(re),
                    Err(e) => {
                        tracing::warn!(rule = %r.label, kind, %e, "invalid exclusion regex dropped");
                        None
                    }
                }
            };
            CompiledRule {
                title_regex: compile(r.title_regex.as_ref(), "title_regex"),
                url_pattern: compile(r.url_pattern.as_ref(), "url_pattern"),
                process: r.process.map(|p| p.to_ascii_lowercase()),
                window_class: r.window_class,
                label: r.label,
            }
        })
        .collect()
}

impl ExclusionList {
    /// Compile a set of rules into a matchable list (doc 05 §4).
    pub fn compile(rules: Vec<ExclusionRule>) -> Self {
        Self { rules: Arc::new(std::sync::RwLock::new(Arc::new(compile_rules(rules)))) }
    }

    /// Recompile and swap the shared rule set — visible to every clone (the
    /// capture sampler + normalizer) on their next check. The caller owns the
    /// fail-open/fail-closed policy: on a source read error, DON'T call this —
    /// keeping the previous list beats silently dropping protections.
    pub fn replace(&self, rules: Vec<ExclusionRule>) {
        let compiled = Arc::new(compile_rules(rules));
        *self.rules.write().unwrap_or_else(|p| p.into_inner()) = compiled;
    }

    /// An **EMPTY** compiled list — the pre-DB bootstrap used by unit tests
    /// and hardware-less harnesses. Out-of-box protection does NOT live here:
    /// it ships as the durable [`SHIPPED_DEFAULT_RULES`] seed (decision #20),
    /// which lands in the `exclusion_list` table and arrives through
    /// [`rules_from_rows`] like any user rule. This stays empty on purpose: a
    /// compiled-in list could never be deleted by the user, and would
    /// resurrect rules they removed.
    pub fn shipped_defaults() -> Self {
        Self::default()
    }

    /// Snapshot the current compiled set (one atomic pointer clone per check).
    fn snapshot(&self) -> Arc<Vec<CompiledRule>> {
        self.rules.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Rule count (diagnostics).
    pub fn len(&self) -> usize {
        self.snapshot().len()
    }

    /// True when no rules are loaded.
    pub fn is_empty(&self) -> bool {
        self.snapshot().is_empty()
    }

    /// Does any rule carry a (compiled) `url_pattern` matcher? Callers that
    /// gate on a browser URL use this to decide whether "no URL resolvable"
    /// must fail closed (the executor's exclusion probe, decision #49).
    pub fn has_url_rules(&self) -> bool {
        self.snapshot().iter().any(|r| r.url_pattern.is_some())
    }

    /// The core predicate (doc 05 §4, doc 13 §4): is this context excluded?
    /// Matched against process / window-class / title / url in that order; the
    /// private-window heuristic runs even with zero rules (doc 13 §4).
    pub fn is_excluded(
        &self,
        process: Option<&str>,
        window_class: Option<&str>,
        title: Option<&str>,
        url: Option<&str>,
    ) -> ExclusionVerdict {
        for rule in self.snapshot().iter() {
            let hit = matches_rule(rule, process, window_class, title, url);
            if hit {
                let mut flags = redaction_flags::EXCLUDED;
                if is_private_window(title) {
                    flags |= redaction_flags::PRIVATE_WINDOW;
                }
                return ExclusionVerdict::Excluded { flags, label: rule.label.clone() };
            }
        }
        if is_private_window(title) {
            return ExclusionVerdict::Excluded {
                flags: redaction_flags::EXCLUDED | redaction_flags::PRIVATE_WINDOW,
                label: "private window".to_string(),
            };
        }
        ExclusionVerdict::Allowed
    }
}

/// Any populated matcher hitting ⇒ the rule matches (doc 05 §4: OR semantics
/// across kinds within one rule — each populated field is an independent match).
fn matches_rule(
    rule: &CompiledRule,
    process: Option<&str>,
    window_class: Option<&str>,
    title: Option<&str>,
    url: Option<&str>,
) -> bool {
    if let (Some(want), Some(got)) = (&rule.process, process) {
        if got.to_ascii_lowercase() == *want {
            return true;
        }
    }
    if let (Some(want), Some(got)) = (&rule.window_class, window_class) {
        if got == want {
            return true;
        }
    }
    if let (Some(re), Some(got)) = (&rule.title_regex, title) {
        if re.is_match(got) {
            return true;
        }
    }
    if let (Some(re), Some(got)) = (&rule.url_pattern, url) {
        if re.is_match(got) {
            return true;
        }
    }
    false
}

/// Map durable `exclusion_list` rows — `(id, match_kind, pattern, enabled)` —
/// into [`ExclusionRule`]s, skipping disabled rows and warning on an unknown
/// kind. ONE definition shared by the startup compile and the hot-reload path
/// (`add_exclusion`/`set_exclusion`), so the two cannot drift.
pub fn rules_from_rows(rows: Vec<(i64, String, String, bool)>) -> Vec<ExclusionRule> {
    rows.into_iter()
        .filter(|(_, _, _, enabled)| *enabled)
        .filter_map(|(_, kind, pattern, _)| {
            let mut rule = ExclusionRule { label: pattern.clone(), ..Default::default() };
            match kind.as_str() {
                "process" => rule.process = Some(pattern),
                "window_class" => rule.window_class = Some(pattern),
                "title_regex" => rule.title_regex = Some(pattern),
                "url_pattern" => rule.url_pattern = Some(pattern),
                other => {
                    tracing::warn!(kind = other, "unknown exclusion match_kind ignored");
                    return None;
                }
            }
            Some(rule)
        })
        .collect()
}

/// The curated out-of-box seed (owner decision #20, Doc 24, 2026-08-16 —
/// supersedes ADR-029/Q15's "defaults ship EMPTY" posture): common password
/// managers plus generic banking-style URL/title patterns. Expressed as the
/// same `(match_kind, pattern)` pairs the durable `exclusion_list` table
/// stores, so seeded rows ride the existing add/disable/delete machinery and
/// appear in the Activity & Privacy manager exactly like user-entered rules —
/// visible, disableable, and permanently deletable.
///
/// Process names are lowercase (process matching is case-insensitive, and
/// detect-and-suggest's catalogue normalizes the same way); regex kinds
/// compile case-insensitively. The banking patterns over-match by design
/// (e.g. any https host containing "bank"): a false hit costs one capture,
/// never privacy — exclusion may only ever fail closed.
pub const SHIPPED_DEFAULT_RULES: &[(&str, &str)] = &[
    // Password managers — desktop apps, by process image name.
    ("process", "1password.exe"),
    ("process", "bitwarden.exe"),
    ("process", "keepass.exe"),
    ("process", "keepassxc.exe"),
    ("process", "lastpass.exe"),
    ("process", "dashlane.exe"),
    // Proton Pass has shipped under both image names.
    ("process", "protonpass.exe"),
    ("process", "proton pass.exe"),
    // Password managers — web vaults (extension/UIA-sourced URLs traverse the
    // same gate, FIX 2.2).
    ("url_pattern", r"^https://([a-z0-9-]+\.)?1password\.(com|ca|eu)/"),
    ("url_pattern", r"^https://vault\.bitwarden\.(com|eu)/"),
    ("url_pattern", r"^https://([a-z0-9-]+\.)?lastpass\.com/"),
    ("url_pattern", r"^https://app\.dashlane\.com/"),
    ("url_pattern", r"^https://pass\.proton\.me/"),
    // Generic banking-style patterns: any https host containing "bank", plus
    // the common online-banking host/path markers, plus the title fallback for
    // when no URL reaches the gate.
    ("url_pattern", r"^https://[^/]*bank[^/]*(/|$)"),
    ("url_pattern", r"^https://[^?#]*(online.?banking|internet.?banking|net.?banking|ebanking)"),
    ("title_regex", r"\bonline banking\b"),
];

/// Settings-table key for the one-shot defaults seed (decision #20). Present ⇒
/// the seed already ran on this install and must never run again — that is
/// what keeps a default the user deleted from resurrecting. The startup wiring
/// writes it (`Db::set_setting`) only AFTER every needed row inserted
/// successfully; an interrupted seed therefore retries next launch, and
/// [`defaults_needing_seed`] makes that retry idempotent.
pub const EXCLUSION_DEFAULTS_SEEDED_KEY: &str = "exclusion_defaults_seeded";

/// Which shipped defaults still need inserting — the seeding brain (decision
/// #20), kept beside the matcher so its semantics are testable without a DB.
/// `rows` are the raw `exclusion_list` rows — `(id, match_kind, pattern,
/// enabled)`, the same shape [`rules_from_rows`] takes — INCLUDING disabled
/// ones.
///
/// - **Seed once, never resurrect:** with `already_seeded` (the
///   [`EXCLUSION_DEFAULTS_SEEDED_KEY`] flag is present) this returns nothing,
///   so a default the user deleted can never reappear on a later launch.
/// - **Idempotent retry:** if the seed was interrupted before the flag write,
///   the next launch retries, but any `(match_kind, pattern)` already present
///   — enabled OR disabled, compared case-insensitively — is skipped. The
///   disabled-row skip matters because `Db::add_exclusion_rule` re-enables on
///   re-add: without it, a retry could silently flip a rule the user had
///   switched off back on.
/// - **Never fail open:** seeding is purely additive and runs before the
///   startup compile; when the caller's own DB read fails it must skip seeding
///   (keeping every existing row intact), never clear or replace anything.
pub fn defaults_needing_seed(
    already_seeded: bool,
    rows: &[(i64, String, String, bool)],
) -> Vec<(&'static str, &'static str)> {
    if already_seeded {
        return Vec::new();
    }
    let existing: std::collections::HashSet<(String, String)> = rows
        .iter()
        .map(|(_, kind, pattern, _)| (kind.to_ascii_lowercase(), pattern.to_ascii_lowercase()))
        .collect();
    SHIPPED_DEFAULT_RULES
        .iter()
        .filter(|(kind, pattern)| {
            !existing.contains(&(kind.to_ascii_lowercase(), pattern.to_ascii_lowercase()))
        })
        .copied()
        .collect()
}

/// Validate one `(match_kind, pattern)` pair BEFORE it is persisted (doc 13 §4).
///
/// [`ExclusionList::compile`] fails **open** on a bad regex — it drops that
/// matcher and keeps going, so one broken rule cannot kill the whole list. That
/// is right at load time and disastrous at *entry* time: a rule whose only
/// matcher fails to compile becomes a rule that matches nothing, while the UI
/// happily shows it as an active protection. The user believes an app is
/// excluded and it is being captured.
///
/// So entry is validated here, with the *same* `RegexBuilder` configuration the
/// compile path uses — one definition, no drift.
pub fn validate_pattern(match_kind: &str, pattern: &str) -> Result<(), String> {
    if pattern.trim().is_empty() {
        return Err("an exclusion pattern cannot be empty".into());
    }
    match match_kind {
        // Literal matchers: any non-empty string is valid.
        "process" | "window_class" => Ok(()),
        "title_regex" | "url_pattern" => {
            regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .map(|_| ())
                .map_err(|e| format!("invalid regex: {e}"))
        }
        other => Err(format!(
            "unknown match_kind `{other}` (expected process | window_class | title_regex | url_pattern)"
        )),
    }
}

/// Heuristic for a private/incognito browser window via title-suffix patterns
/// (doc 13 §4). Treated as excluded with the [`redaction_flags::PRIVATE_WINDOW`]
/// bit. [VERIFY reliability per browser — suffixes drift across versions/locales
/// (RK4-adjacent); the en-US suffixes below are the shipping set, extended from
/// settings at M9.]
pub fn is_private_window(title: Option<&str>) -> bool {
    let Some(t) = title else { return false };
    const PRIVATE_SUFFIXES: [&str; 6] = [
        "- incognito",            // Chrome/Brave/Opera en-US
        "[inprivate]",            // Edge en-US
        "- inprivate",            // Edge variants
        "(private browsing)",     // Firefox en-US
        "private browsing",       // Firefox variants
        "— private browsing",     // Firefox em-dash variant
    ];
    let t = t.trim().to_ascii_lowercase();
    PRIVATE_SUFFIXES.iter().any(|s| t.ends_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_shipped_defaults_stay_empty_seed_is_durable() {
        // The compiled bootstrap stays EMPTY on purpose: out-of-box protection
        // ships as the durable seed (decision #20), so the user can disable or
        // permanently delete every rule. A compiled-in list could do neither —
        // it would shadow the manager and resurrect deleted rules.
        let list = ExclusionList::shipped_defaults();
        assert!(list.is_empty(), "compiled form is empty; protection is the durable seed");
        assert_eq!(
            list.is_excluded(Some("1password.exe"), None, None, None),
            ExclusionVerdict::Allowed,
            "nothing is compiled in — seeded rows arrive via rules_from_rows"
        );
    }

    #[test]
    fn shipped_default_rules_all_validate_compile_and_match() {
        // Every seed row must survive the same entry validation user rules get
        // — `compile` fails open on a bad matcher, so a non-validating default
        // would ship as a rule the UI lists but which protects nothing.
        for (kind, pattern) in SHIPPED_DEFAULT_RULES {
            validate_pattern(kind, pattern)
                .unwrap_or_else(|e| panic!("shipped default {kind}:{pattern}: {e}"));
        }

        // Compile through the SAME row pipeline the DB path uses; none may drop.
        let rows: Vec<(i64, String, String, bool)> = SHIPPED_DEFAULT_RULES
            .iter()
            .enumerate()
            .map(|(i, (k, p))| (i as i64, k.to_string(), p.to_string(), true))
            .collect();
        let list = ExclusionList::compile(rules_from_rows(rows));
        assert_eq!(list.len(), SHIPPED_DEFAULT_RULES.len(), "no default dropped at compile");

        // Spot-check each family's intent.
        assert!(list.is_excluded(Some("KeePassXC.exe"), None, None, None).is_excluded());
        assert!(list
            .is_excluded(Some("chrome.exe"), None, Some("Vault"), Some("https://vault.bitwarden.com/#/login"))
            .is_excluded());
        assert!(list
            .is_excluded(Some("chrome.exe"), None, Some("tab"), Some("https://www.bankofamerica.com/"))
            .is_excluded());
        assert!(list
            .is_excluded(
                Some("chrome.exe"),
                None,
                Some("tab"),
                Some("https://www.chase.com/personal/online-banking")
            )
            .is_excluded());
        assert!(list
            .is_excluded(Some("firefox.exe"), None, Some("Online Banking — Acme CU"), None)
            .is_excluded());
        // Ordinary work stays untouched.
        assert_eq!(
            list.is_excluded(Some("code.exe"), None, Some("main.rs — aperture"), Some("https://docs.rs/regex")),
            ExclusionVerdict::Allowed
        );
    }

    #[test]
    fn defaults_seed_once_and_never_resurrect() {
        // Fresh install: no flag, empty table → the full curated set.
        assert_eq!(defaults_needing_seed(false, &[]).len(), SHIPPED_DEFAULT_RULES.len());

        // The user deletes a default (its row is GONE). With the flag set,
        // later launches must not bring it back — decision #20's critical
        // no-resurrection semantic.
        let after_delete: Vec<(i64, String, String, bool)> = SHIPPED_DEFAULT_RULES
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, (k, p))| (i as i64, k.to_string(), p.to_string(), true))
            .collect();
        assert!(
            defaults_needing_seed(true, &after_delete).is_empty(),
            "a deleted default must stay deleted"
        );
        // Even deleting EVERY rule must not re-seed once the flag is set.
        assert!(
            defaults_needing_seed(true, &[]).is_empty(),
            "an emptied rule list must not resurrect the defaults"
        );
    }

    #[test]
    fn interrupted_seed_retry_is_idempotent_and_never_reenables() {
        // A crash before the flag write retries the seed next launch. Rows
        // already present are skipped — INCLUDING disabled ones and case
        // variants — because `Db::add_exclusion_rule` re-enables on re-add,
        // and a retry must never flip a user-disabled rule back on.
        let rows = vec![
            (1_i64, "process".to_string(), "1Password.exe".to_string(), false), // disabled + case-variant
            (2_i64, "process".to_string(), "bitwarden.exe".to_string(), true),
        ];
        let need = defaults_needing_seed(false, &rows);
        assert_eq!(need.len(), SHIPPED_DEFAULT_RULES.len() - 2);
        assert!(
            !need.iter().any(|(k, p)| *k == "process" && p.eq_ignore_ascii_case("1password.exe")),
            "a disabled default must not be re-added (re-add would re-enable it)"
        );
        assert!(!need.iter().any(|(k, p)| *k == "process" && *p == "bitwarden.exe"));
    }

    #[test]
    fn process_and_class_and_title_and_url_kinds_match() {
        let list = ExclusionList::compile(vec![
            ExclusionRule {
                process: Some("1Password.exe".into()),
                label: "1Password".into(),
                ..Default::default()
            },
            ExclusionRule {
                title_regex: Some(r"mybank\.example".into()),
                label: "MyBank".into(),
                ..Default::default()
            },
            ExclusionRule {
                url_pattern: Some(r"^https://banking\.".into()),
                label: "banking domain".into(),
                ..Default::default()
            },
        ]);

        // process, case-insensitive
        let v = list.is_excluded(Some("1PASSWORD.EXE"), None, None, None);
        assert!(matches!(&v, ExclusionVerdict::Excluded { label, flags }
            if label == "1Password" && *flags == redaction_flags::EXCLUDED));

        // title regex
        assert!(list
            .is_excluded(Some("chrome.exe"), None, Some("Login — mybank.example"), None)
            .is_excluded());

        // url_pattern (ADR-040 — extension-sourced URLs traverse this gate, FIX 2.2)
        assert!(list
            .is_excluded(Some("chrome.exe"), None, Some("Bank"), Some("https://banking.acme.test/x"))
            .is_excluded());

        // no match
        assert_eq!(
            list.is_excluded(Some("code.exe"), None, Some("main.rs"), None),
            ExclusionVerdict::Allowed
        );
    }

    #[test]
    fn has_url_rules_reflects_compiled_url_pattern_matchers_only() {
        assert!(!ExclusionList::shipped_defaults().has_url_rules(), "empty list");
        let process_only = ExclusionList::compile(vec![ExclusionRule {
            process: Some("1password.exe".into()),
            label: "1Password".into(),
            ..Default::default()
        }]);
        assert!(!process_only.has_url_rules());
        let with_url = ExclusionList::compile(vec![ExclusionRule {
            url_pattern: Some(r"^https://banking\.".into()),
            label: "banking".into(),
            ..Default::default()
        }]);
        assert!(with_url.has_url_rules());
        // A url_pattern that failed to compile was dropped: it is not a rule.
        let broken = ExclusionList::compile(vec![ExclusionRule {
            url_pattern: Some("([unclosed".into()),
            label: "broken".into(),
            ..Default::default()
        }]);
        assert!(!broken.has_url_rules());
        // `replace` swaps the shared set: every clone sees the new answer.
        let clone = process_only.clone();
        process_only.replace(vec![ExclusionRule {
            url_pattern: Some("^https://x".into()),
            label: "x".into(),
            ..Default::default()
        }]);
        assert!(clone.has_url_rules());
    }

    #[test]
    fn private_windows_are_excluded_even_with_zero_rules() {
        let list = ExclusionList::shipped_defaults();
        let v = list.is_excluded(Some("chrome.exe"), None, Some("secret stuff - Incognito"), None);
        match v {
            ExclusionVerdict::Excluded { flags, .. } => {
                assert_ne!(flags & redaction_flags::PRIVATE_WINDOW, 0);
                assert_ne!(flags & redaction_flags::EXCLUDED, 0);
            }
            _ => panic!("incognito must be excluded (doc 13 §4)"),
        }
        assert!(is_private_window(Some("x (Private Browsing)")));
        assert!(is_private_window(Some("tab [InPrivate]")));
        assert!(!is_private_window(Some("Incognito mode explained - Chrome")));
    }

    #[test]
    fn validate_pattern_rejects_at_entry_what_compile_would_silently_drop() {
        // The exact hazard: `compile` fails open, so an unvalidated bad regex
        // becomes a rule the UI shows as active but which protects nothing.
        assert!(validate_pattern("title_regex", "([unclosed").is_err());
        assert!(validate_pattern("url_pattern", "*bad").is_err());
        assert!(validate_pattern("process", "").is_err(), "empty is never a rule");
        assert!(validate_pattern("process", "   ").is_err(), "whitespace is not a pattern");
        assert!(validate_pattern("nonsense", "x").is_err(), "unknown kind rejected");

        // Valid ones pass, including literals that are not valid regexes.
        assert!(validate_pattern("process", "1Password.exe").is_ok());
        assert!(validate_pattern("window_class", "([not-a-regex").is_ok(), "literal kinds are not regexes");
        assert!(validate_pattern("title_regex", r"mybank\.example").is_ok());
        assert!(validate_pattern("url_pattern", r"^https://banking\.").is_ok());
    }

    #[test]
    fn invalid_regex_fails_open_for_that_matcher_only() {
        let list = ExclusionList::compile(vec![ExclusionRule {
            title_regex: Some("([unclosed".into()),
            process: Some("evil.exe".into()),
            label: "broken".into(),
            ..Default::default()
        }]);
        // The bad regex was dropped, the process matcher still works.
        assert!(list.is_excluded(Some("evil.exe"), None, None, None).is_excluded());
        assert_eq!(
            list.is_excluded(None, None, Some("([unclosed"), None),
            ExclusionVerdict::Allowed
        );
    }
}
