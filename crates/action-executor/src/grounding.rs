//! Label grounding — the pure half of Doc 22 §6 ("Claude says *Submit*;
//! which UIA element is that?"). Everything here is string logic with no OS
//! dependency so Q-V2-01 (is fuzzy matching sufficient?) can be exercised in
//! plain unit tests; the UIA walk that feeds candidate names lives in
//! [`crate::platform`].
//!
//! Ranking (Doc 22 §6, Doc 24 #51): exact → contains → Levenshtein ≤ 2, all on
//! the [`normalize_label`]d form. A fuzzy hit on a *consequential* label is
//! exactly what decision #51's secondary check exists to catch, so the quality
//! is reported rather than collapsed to a bool.

use std::cmp::Ordering;

/// Largest edit distance still accepted by [`match_label`] (Doc 22 §6
/// [ASSUMPTION], to be confirmed by the V2-M0 spike / Q-V2-01).
pub const MAX_FUZZY_DISTANCE: usize = 2;

/// Shortest normalized label that may match by containment — anything shorter
/// ("OK", "x") is far too easy to find inside an unrelated name.
pub const MIN_CONTAINS_LEN: usize = 3;

/// Shortest normalized label that may match fuzzily: two edits rewrite a
/// two-letter label entirely ("OK" → "No"), so short labels match exactly or
/// not at all.
pub const MIN_FUZZY_LEN: usize = 4;

/// Canonical form used for every comparison: lowercase, `&`-accelerators
/// stripped (`"&File"` → `"file"`, `"&&"` → `"&"`), a trailing `...`/`…`
/// removed (`"Save As..."` → `"save as"`), whitespace collapsed to single
/// spaces and trimmed.
pub fn normalize_label(raw: &str) -> String {
    // 1. Accelerator markers: a single '&' is a mnemonic prefix, '&&' is a
    //    literal ampersand.
    let mut unaccel = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                chars.next();
                unaccel.push('&');
            }
            continue;
        }
        unaccel.push(c);
    }
    // 2. Trailing ellipsis ("Open…", "Save As...") is menu decoration.
    let mut trimmed = unaccel.trim();
    loop {
        if let Some(rest) = trimmed.strip_suffix("...") {
            trimmed = rest.trim_end();
        } else if let Some(rest) = trimmed.strip_suffix('…') {
            trimmed = rest.trim_end();
        } else {
            break;
        }
    }
    // 3. Case + whitespace.
    trimmed
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Classic Levenshtein edit distance over Unicode scalar values.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// How a candidate label matched the target. Ordered by preference:
/// `Exact > Contains > Fuzzy(d)` with a smaller `d` ranking higher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchQuality {
    /// Normalized forms are identical.
    Exact,
    /// One normalized form contains the other (both ≥ [`MIN_CONTAINS_LEN`]).
    Contains,
    /// Levenshtein distance between the normalized forms (≤ [`MAX_FUZZY_DISTANCE`]).
    Fuzzy(usize),
}

impl MatchQuality {
    /// Higher is better; used by the `Ord` impl.
    fn rank(self) -> (u8, isize) {
        match self {
            MatchQuality::Exact => (2, 0),
            MatchQuality::Contains => (1, 0),
            MatchQuality::Fuzzy(d) => (0, -(d as isize)),
        }
    }

    /// Short tag for the outcome description ("exact" / "contains" / "fuzzy:1").
    pub fn label(self) -> String {
        match self {
            MatchQuality::Exact => "exact".to_string(),
            MatchQuality::Contains => "contains".to_string(),
            MatchQuality::Fuzzy(d) => format!("fuzzy:{d}"),
        }
    }
}

impl PartialOrd for MatchQuality {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MatchQuality {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank().cmp(&other.rank())
    }
}

/// Does `candidate` (a UIA name / window title) match Claude's `target`?
/// Returns the quality of the best applicable rule, or `None`.
pub fn match_label(candidate: &str, target: &str) -> Option<MatchQuality> {
    let c = normalize_label(candidate);
    let t = normalize_label(target);
    if t.is_empty() || c.is_empty() {
        return None;
    }
    if c == t {
        return Some(MatchQuality::Exact);
    }
    let shorter = c.chars().count().min(t.chars().count());
    if shorter >= MIN_CONTAINS_LEN && (c.contains(&t) || t.contains(&c)) {
        return Some(MatchQuality::Contains);
    }
    if shorter < MIN_FUZZY_LEN {
        return None;
    }
    let d = levenshtein(&c, &t);
    (d <= MAX_FUZZY_DISTANCE).then_some(MatchQuality::Fuzzy(d))
}

/// Index + quality of the best-matching label; the first of equal-quality
/// candidates wins (UIA FindAll returns document order, so "first" is the
/// top-most/left-most — the least surprising pick).
pub fn best_index<'a>(
    labels: impl IntoIterator<Item = &'a str>,
    target: &str,
) -> Option<(usize, MatchQuality)> {
    let mut best: Option<(usize, MatchQuality)> = None;
    for (i, label) in labels.into_iter().enumerate() {
        if let Some(q) = match_label(label, target) {
            if best.is_none_or(|(_, bq)| q > bq) {
                best = Some((i, q));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_accelerators_ellipsis_case_and_whitespace() {
        assert_eq!(normalize_label("&File"), "file");
        assert_eq!(normalize_label("Save &As..."), "save as");
        assert_eq!(normalize_label("Open…"), "open");
        assert_eq!(normalize_label("  Don't   Save \n"), "don't save");
        assert_eq!(normalize_label("Fish && Chips"), "fish & chips");
        assert_eq!(normalize_label("......"), "");
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("submit", "sumbit"), 2);
        assert_eq!(levenshtein("héllo", "hello"), 1, "chars, not bytes");
    }

    #[test]
    fn match_ladder_exact_then_contains_then_fuzzy() {
        assert_eq!(match_label("&Submit", "submit"), Some(MatchQuality::Exact));
        assert_eq!(
            match_label("Submit order", "Submit"),
            Some(MatchQuality::Contains)
        );
        assert_eq!(
            match_label("Save", "Save changes"),
            Some(MatchQuality::Contains),
            "containment works in both directions"
        );
        assert_eq!(match_label("Sumbit", "Submit"), Some(MatchQuality::Fuzzy(2)));
        assert_eq!(match_label("Cancel", "Submit"), None);
        assert_eq!(match_label("", "Submit"), None);
        assert_eq!(match_label("Submit", ""), None);
    }

    #[test]
    fn short_labels_match_exactly_or_not_at_all() {
        // "OK" is inside "BOOK" — but two letters is not a grounding signal.
        assert_eq!(match_label("Book", "OK"), None);
        // Two edits rewrite "OK" into "No": never a fuzzy hit on short labels.
        assert_eq!(match_label("No", "OK"), None);
        assert_eq!(match_label("Yes", "Yet"), None, "3 letters: no fuzzy either");
        assert_eq!(match_label("OK", "ok"), Some(MatchQuality::Exact));
        // Three letters may match by containment; four may match fuzzily.
        assert_eq!(match_label("Yes, do it", "Yes"), Some(MatchQuality::Contains));
        assert_eq!(match_label("Sane", "Save"), Some(MatchQuality::Fuzzy(1)));
    }

    #[test]
    fn quality_ordering_prefers_exact_then_contains_then_closer_fuzzy() {
        assert!(MatchQuality::Exact > MatchQuality::Contains);
        assert!(MatchQuality::Contains > MatchQuality::Fuzzy(0));
        assert!(MatchQuality::Fuzzy(1) > MatchQuality::Fuzzy(2));
        assert_eq!(MatchQuality::Fuzzy(1).label(), "fuzzy:1");
    }

    #[test]
    fn best_index_picks_highest_quality_first_on_ties() {
        let labels = ["Sumbit", "Submit order", "Submit", "Submit"];
        assert_eq!(best_index(labels, "Submit"), Some((2, MatchQuality::Exact)));
        assert_eq!(best_index(["Cancel", "Close"], "Submit"), None);
        assert_eq!(
            best_index(["Notepad", "Untitled - Notepad"], "Notepad"),
            Some((0, MatchQuality::Exact))
        );
    }
}
