//! The redaction pipeline (doc 13 §5).
//!
//! Ordered, deterministic rules run over **every text item** of a
//! [`ContextPayload`] at payload assembly, **before** the preview is rendered
//! ("preview == wire", doc 13 §3). Each hit is replaced with a typed placeholder
//! (`⟨email#1⟩`) and increments the matching [`Redaction`] entry, which the
//! preview shows as `rule + count` (doc 13 §5).
//!
//! Rule order is load-bearing — narrower/higher-risk shapes first so e.g. an
//! `sk-…` key is caught as a secret before a later rule could mangle it:
//!
//! | Order | [`RuleKind`]    | Mechanism (doc 13 §5)                              |
//! |------:|-----------------|----------------------------------------------------|
//! | 1     | `SecretKey`     | regex: AWS keys, `sk-…`, GitHub PATs, Slack `xox…`, `Authorization: Bearer` headers, PEM/OPENSSH key bodies, JWT |
//! | 2     | `PaymentCard`   | 13–19 digit runs passing the Luhn check            |
//! | 3     | `Iban`          | country-prefixed IBAN regex                        |
//! | 4     | `Email`         | RFC-lite regex                                     |
//! | 5     | `Phone`         | E.164-ish + common local formats                   |
//! | 6     | `UserDefined`   | literal/regex terms from settings                  |
//!
//! Misses are mitigated by the preview's per-item remove/edit affordance — the
//! human is the last redactor by design (doc 13 §5, §9).

use std::collections::HashMap;

use aperture_contracts::context_payload::{ContextPayload, PayloadItem, Redaction};
use regex::Regex;

use crate::PrivacyError;

/// The fixed taxonomy of redaction rule kinds (doc 13 §5). Ordering of the
/// variants matches the pipeline order; [`RuleKind::ORDERED`] is the source of
/// truth for execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleKind {
    /// 1 — secrets/keys: AWS access keys, `sk-…` tokens, GitHub PATs, Slack
    /// tokens, `Authorization: Bearer` headers, PEM/OPENSSH key bodies, JWTs.
    SecretKey,
    /// 2 — payment cards: 13–19 digit runs passing Luhn.
    PaymentCard,
    /// 3 — IBAN / account-like: country-prefixed IBAN.
    Iban,
    /// 4 — email addresses (RFC-lite).
    Email,
    /// 5 — phone numbers (E.164-ish + local).
    Phone,
    /// 6 — user-defined literal/regex terms from settings.
    UserDefined,
}

impl RuleKind {
    /// The pipeline order (doc 13 §5). Built-in rules 1–5 always run; rule 6 runs
    /// last over the user's configured terms.
    pub const ORDERED: [RuleKind; 6] = [
        RuleKind::SecretKey,
        RuleKind::PaymentCard,
        RuleKind::Iban,
        RuleKind::Email,
        RuleKind::Phone,
        RuleKind::UserDefined,
    ];

    /// The stable label written into [`Redaction::rule`] and shown in the preview.
    pub fn label(self) -> &'static str {
        match self {
            RuleKind::SecretKey => "secret_key",
            RuleKind::PaymentCard => "payment_card",
            RuleKind::Iban => "iban",
            RuleKind::Email => "email",
            RuleKind::Phone => "phone",
            RuleKind::UserDefined => "user_defined",
        }
    }

    /// The placeholder noun used inside `⟨…#n⟩`, e.g. `email` -> `⟨email#1⟩`.
    pub fn placeholder_noun(self) -> &'static str {
        match self {
            RuleKind::SecretKey => "secret",
            RuleKind::PaymentCard => "card",
            RuleKind::Iban => "iban",
            RuleKind::Email => "email",
            RuleKind::Phone => "phone",
            RuleKind::UserDefined => "term",
        }
    }
}

/// One byte range of an input string that a redaction rule would replace
/// (doc 24 decision #3). Produced by [`Redactor::find_spans`]; consumed by the
/// image-redaction gate ([`crate::image_redaction`]) to decide which OCR word
/// boxes to paint over, so the screenshot path reuses the text rules verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionSpan {
    /// The rule that claimed the range (earlier [`RuleKind::ORDERED`] rules win).
    pub rule: RuleKind,
    /// Start byte offset into the input (inclusive).
    pub start: usize,
    /// End byte offset into the input (exclusive).
    pub end: usize,
}

/// A user-defined redaction term (doc 13 §5 rule 6, from settings).
#[derive(Debug, Clone)]
pub struct UserTerm {
    /// The literal string or regex source.
    pub pattern: String,
    /// `true` => `pattern` is a regex; `false` => match literally.
    pub is_regex: bool,
}

/// The compiled rule set. Built-in regexes are compiled once (they are constant
/// and cannot fail); user terms are compiled from settings and *can* fail
/// ([`PrivacyError::InvalidRule`]).
///
/// NOTE (milestone): the full privacy subsystem is M9, but the reasoning gateway
/// (M7) structurally needs redaction-**before**-preview (doc 09 §5, doc 13 §5) —
/// a no-op here would silently egress raw secrets. So this pipeline is implemented
/// at M7; the M9-scoped privacy pieces (consent UI, key manager, audit-row DB
/// persistence, exclusion manager) remain their `todo!("M9")` stubs.
pub struct Redactor {
    /// Rule 1 — secrets: AWS access keys, `sk-…` tokens, GitHub PATs, Slack
    /// tokens, `Authorization: Bearer` headers, PEM/OPENSSH key bodies, JWTs.
    secret: Vec<Regex>,
    /// Rule 2 — candidate 13–19 digit runs (Luhn-filtered before redaction).
    card: Regex,
    /// Rule 3 — country-prefixed IBAN.
    iban: Regex,
    /// Rule 4 — email (RFC-lite).
    email: Regex,
    /// Rule 5 — phone (E.164-ish + local).
    phone: Regex,
    /// Rule 6 — user-defined terms, already compiled (literals escaped).
    user_terms: Vec<Regex>,
}

impl Redactor {
    /// Build the redactor with the built-in rules plus the user's configured
    /// terms (doc 13 §5 rule 6). Returns [`PrivacyError::InvalidRule`] if a
    /// user-supplied regex does not compile. The built-in patterns are constants
    /// and are `expect`-compiled (a failure would be a build-time bug, not runtime).
    pub fn new(user_terms: &[UserTerm]) -> Result<Self, PrivacyError> {
        let secret = vec![
            // Full PEM/OPENSSH private-key BODIES must run before the header-only
            // fallback: a complete key (header + base64 body + END line) collapses
            // to a single placeholder; non-greedy so two keys stay two hits.
            Regex::new(
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            )
            .expect("pem body regex"),
            // Header-only fallback: a truncated capture (no END line) still redacts.
            Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("pem header regex"),
            Regex::new(r"AKIA[0-9A-Z]{16}").expect("aws key regex"),
            Regex::new(r"sk-[A-Za-z0-9]{16,}").expect("sk token regex"),
            Regex::new(r"eyJ[A-Za-z0-9_-]{6,}\.eyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}")
                .expect("jwt regex"),
            // GitHub PATs: classic (ghp_/gho_/ghu_/ghs_/ghr_ + 36 alnum) and
            // fine-grained (github_pat_ + 82 [alnum_]); floors keep short
            // prose like "ghp_test" from false-firing.
            Regex::new(r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,}").expect("github pat regex"),
            Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{36,}").expect("github fg pat regex"),
            // Slack tokens: xoxb-/xoxp-/xoxa-/xoxr-/xoxs- + digit/dash sections.
            Regex::new(r"\bxox[abprs]-[A-Za-z0-9-]{10,}").expect("slack token regex"),
            // `Authorization: Bearer <token>` — anchored to the literal header name
            // (same line only) so prose containing the word "Bearer" never matches;
            // token chars per RFC 6750 b64token, floored at 16.
            Regex::new(r"(?i:authorization)[ \t]*:[ \t]*(?i:bearer)[ \t]+[A-Za-z0-9._~+/-]{16,}=*")
                .expect("bearer header regex"),
        ];
        // A digit-started run of 13–19 digits allowing single space/dash separators;
        // Luhn filters false positives (long order/ID numbers) at replacement time.
        let card = Regex::new(r"\b\d(?:[ -]?\d){12,18}\b").expect("card regex");
        let iban = Regex::new(r"\b[A-Z]{2}\d{2}[A-Z0-9]{10,30}\b").expect("iban regex");
        let email =
            Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b").expect("email regex");
        // Loose phone matcher — over-matches are mitigated by the preview's per-item
        // remove/edit affordance (doc 13 §5: the human is the last redactor).
        let phone = Regex::new(r"\+?\d[\d ().-]{6,}\d").expect("phone regex");

        let mut user = Vec::with_capacity(user_terms.len());
        for term in user_terms {
            let src = if term.is_regex {
                term.pattern.clone()
            } else {
                regex::escape(&term.pattern)
            };
            let re = Regex::new(&src).map_err(|source| PrivacyError::InvalidRule {
                rule: term.pattern.clone(),
                source,
            })?;
            user.push(re);
        }
        Ok(Self { secret, card, iban, email, phone, user_terms: user })
    }

    /// Redact a payload in place: run [`RuleKind::ORDERED`] over every text-bearing
    /// item, replacing hits with typed placeholders and accumulating the per-rule
    /// counts into `payload.redactions`. Runs at assembly, **before** preview.
    ///
    /// `OcrText` items whose text changed are flagged `redacted = true`. Returns
    /// the redactions for convenience (also merged onto the payload, additively).
    ///
    /// INVARIANT (2): this mutates an in-process object only; nothing here egresses.
    pub fn redact_payload(&self, payload: &mut ContextPayload) -> Vec<Redaction> {
        let mut counts: HashMap<&'static str, u32> = HashMap::new();
        for item in &mut payload.items {
            match item {
                PayloadItem::OcrText { text, redacted, .. } => {
                    let (red, hits) = self.redact_text(text);
                    if !hits.is_empty() {
                        *text = red;
                        *redacted = true;
                        merge(&mut counts, hits);
                    }
                }
                PayloadItem::UserAddition { text } => {
                    let (red, hits) = self.redact_text(text);
                    if !hits.is_empty() {
                        *text = red;
                        merge(&mut counts, hits);
                    }
                }
                PayloadItem::EventTrail { events } => {
                    for ev in events.iter_mut() {
                        self.redact_value(ev, &mut counts);
                    }
                }
                PayloadItem::Connector { payload: value, .. } => {
                    self.redact_value(value, &mut counts);
                }
                // Screenshots carry no redactable text here (doc 09 §5).
                PayloadItem::Screenshot { .. } => {}
            }
        }
        // Emit in pipeline order for a deterministic preview (doc 13 §5).
        let result: Vec<Redaction> = RuleKind::ORDERED
            .iter()
            .filter_map(|k| {
                counts
                    .get(k.label())
                    .filter(|&&n| n > 0)
                    .map(|&n| Redaction { rule: k.label().to_string(), count: n })
            })
            .collect();
        payload.redactions.extend(result.iter().cloned());
        result
    }

    /// Redact a single string, returning the rewritten text plus the per-rule hit
    /// counts. The `⟨noun#n⟩` numbering is 1-based per rule kind within this call.
    pub fn redact_text(&self, input: &str) -> (String, Vec<Redaction>) {
        let mut text = input.to_string();
        let mut out = Vec::new();
        for kind in RuleKind::ORDERED {
            let mut count = 0u32;
            let noun = kind.placeholder_noun();
            text = match kind {
                RuleKind::SecretKey => {
                    let mut t = text;
                    for re in &self.secret {
                        t = apply_rule(&t, re, noun, &mut count, |_| true);
                    }
                    t
                }
                RuleKind::PaymentCard => {
                    apply_rule(&text, &self.card, noun, &mut count, |m| luhn_valid(m))
                }
                RuleKind::Iban => apply_rule(&text, &self.iban, noun, &mut count, |_| true),
                RuleKind::Email => apply_rule(&text, &self.email, noun, &mut count, |_| true),
                // Only treat a run as a phone if it is *formatted* (leading `+` or
                // a separator). A bare long digit run is more likely an order/ID
                // number — left for the Luhn-gated card rule + the human reviewer,
                // never falsely scrubbed as a phone (doc 13 §5).
                RuleKind::Phone => apply_rule(&text, &self.phone, noun, &mut count, |m| {
                    m.contains(['+', ' ', '(', ')', '-', '.'])
                }),
                RuleKind::UserDefined => {
                    let mut t = text;
                    for re in &self.user_terms {
                        t = apply_rule(&t, re, noun, &mut count, |_| true);
                    }
                    t
                }
            };
            if count > 0 {
                out.push(Redaction { rule: kind.label().to_string(), count });
            }
        }
        (text, out)
    }

    /// Locate, without rewriting, the byte ranges of `input` that
    /// [`Redactor::redact_text`] would replace (doc 24 decision #3).
    ///
    /// Applies the same rules with the same gates — every secret pattern, Luhn
    /// for cards, the formatted-only phone gate, IBAN, email, user terms — in
    /// [`RuleKind::ORDERED`] order. Spans never overlap: a later rule's match
    /// that touches bytes an earlier rule already claimed is dropped (earlier
    /// rules win, mirroring the placeholder having replaced those bytes before
    /// the later rule ran). Zero-length matches are skipped (nothing to cover).
    /// The result is sorted by `start`.
    ///
    /// Known, accepted divergence from `redact_text`: that function re-scans the
    /// *rewritten* text, so a `\b` created next to a placeholder could in
    /// principle let a later rule match bytes that are not word-bounded in the
    /// original; for the image gate that only ever means a box is left unpainted
    /// next to one that is painted, and the human preview remains the last
    /// redactor (doc 13 §5).
    pub fn find_spans(&self, input: &str) -> Vec<RedactionSpan> {
        let mut spans: Vec<RedactionSpan> = Vec::new();
        for kind in RuleKind::ORDERED {
            match kind {
                RuleKind::SecretKey => {
                    for re in &self.secret {
                        collect_spans(&mut spans, kind, re, input, |_| true);
                    }
                }
                RuleKind::PaymentCard => {
                    collect_spans(&mut spans, kind, &self.card, input, luhn_valid)
                }
                RuleKind::Iban => collect_spans(&mut spans, kind, &self.iban, input, |_| true),
                RuleKind::Email => collect_spans(&mut spans, kind, &self.email, input, |_| true),
                RuleKind::Phone => collect_spans(&mut spans, kind, &self.phone, input, |m| {
                    m.contains(['+', ' ', '(', ')', '-', '.'])
                }),
                RuleKind::UserDefined => {
                    for re in &self.user_terms {
                        collect_spans(&mut spans, kind, re, input, |_| true);
                    }
                }
            }
        }
        spans.sort_by_key(|s| s.start);
        spans
    }

    /// Recursively redact every JSON string value in place (the stringly fields of
    /// `EventTrail` / `Connector` items), accumulating per-rule counts.
    fn redact_value(&self, value: &mut serde_json::Value, counts: &mut HashMap<&'static str, u32>) {
        match value {
            serde_json::Value::String(s) => {
                let (red, hits) = self.redact_text(s);
                if !hits.is_empty() {
                    *s = red;
                    merge(counts, hits);
                }
            }
            serde_json::Value::Array(a) => {
                a.iter_mut().for_each(|v| self.redact_value(v, counts))
            }
            serde_json::Value::Object(o) => {
                o.values_mut().for_each(|v| self.redact_value(v, counts))
            }
            // PII carried as a JSON *number* (e.g. a card number as an integer) is
            // still user data — stringify, redact, and if a rule fired, replace the
            // node with the redacted string so it cannot egress unscrubbed.
            serde_json::Value::Number(_) => {
                let (red, hits) = self.redact_text(&value.to_string());
                if !hits.is_empty() {
                    *value = serde_json::Value::String(red);
                    merge(counts, hits);
                }
            }
            _ => {}
        }
    }
}

/// Replace each match of `re` in `text` (for which `keep(match) == true`) with a
/// `⟨noun#n⟩` placeholder, incrementing `count` per replaced hit.
fn apply_rule(
    text: &str,
    re: &Regex,
    noun: &str,
    count: &mut u32,
    mut keep: impl FnMut(&str) -> bool,
) -> String {
    re.replace_all(text, |caps: &regex::Captures| {
        let matched = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        if keep(matched) {
            *count += 1;
            placeholder(noun, *count)
        } else {
            matched.to_string()
        }
    })
    .into_owned()
}

/// Span-finding twin of [`apply_rule`]: push every non-empty match of `re` in
/// `text` that passes `keep` **and** does not overlap a span already claimed by
/// an earlier rule/pattern (doc 24 decision #3).
fn collect_spans(
    spans: &mut Vec<RedactionSpan>,
    kind: RuleKind,
    re: &Regex,
    text: &str,
    mut keep: impl FnMut(&str) -> bool,
) {
    for m in re.find_iter(text) {
        if m.start() == m.end() || !keep(m.as_str()) {
            continue;
        }
        if spans.iter().any(|s| s.start < m.end() && m.start() < s.end) {
            continue;
        }
        spans.push(RedactionSpan { rule: kind, start: m.start(), end: m.end() });
    }
}

/// Fold a string's per-rule hit counts into the running accumulator, keyed by the
/// stable [`RuleKind::label`] (interned as `&'static str`).
fn merge(counts: &mut HashMap<&'static str, u32>, hits: Vec<Redaction>) {
    for hit in hits {
        // Map the owned label back to its interned &'static str.
        if let Some(kind) = RuleKind::ORDERED.iter().find(|k| k.label() == hit.rule) {
            *counts.entry(kind.label()).or_insert(0) += hit.count;
        }
    }
}

/// Luhn checksum used by rule 2 (payment cards, doc 13 §5). A digit run is only
/// redacted when it passes Luhn AND has 13–19 digits — this keeps long ID/order
/// numbers from being falsely scrubbed. Non-digit separators are ignored.
pub fn luhn_valid(digits: &str) -> bool {
    let ds: Vec<u32> = digits
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| (b - b'0') as u32)
        .collect();
    if !(13..=19).contains(&ds.len()) {
        return false;
    }
    let sum: u32 = ds
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                let v = d * 2;
                if v > 9 {
                    v - 9
                } else {
                    v
                }
            } else {
                d
            }
        })
        .sum();
    sum % 10 == 0
}

/// Format a typed placeholder, e.g. `("email", 1) -> "⟨email#1⟩"` (doc 13 §5).
pub fn placeholder(noun: &str, n: u32) -> String {
    format!("⟨{noun}#{n}⟩")
}

/// Convenience: classify the text-bearing slots of a [`PayloadItem`] so the
/// pipeline knows what to walk. (Screenshots carry no redactable text here;
/// enrichment image safety is handled upstream, doc 09 §5.)
pub fn item_has_text(item: &PayloadItem) -> bool {
    matches!(
        item,
        PayloadItem::OcrText { .. }
            | PayloadItem::UserAddition { .. }
            | PayloadItem::EventTrail { .. }
            | PayloadItem::Connector { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::context_payload::{Intent, TransportTarget};
    use uuid::Uuid;

    fn redactor() -> Redactor {
        Redactor::new(&[]).expect("built-in rules compile")
    }

    #[test]
    fn luhn_accepts_a_valid_card_and_rejects_short_or_bad_runs() {
        assert!(luhn_valid("4111111111111111"), "a valid 16-digit Visa test number");
        assert!(luhn_valid("4111 1111 1111 1111"), "separators are ignored");
        assert!(!luhn_valid("4111111111111112"), "fails the checksum");
        assert!(!luhn_valid("123456789"), "too short (< 13 digits)");
        assert!(!luhn_valid("12345678901234567890"), "too long (> 19 digits)");
    }

    #[test]
    fn redacts_email_and_reports_the_count() {
        let (red, hits) = redactor().redact_text("mail me at a@b.com or c@d.org");
        assert!(red.contains("⟨email#1⟩") && red.contains("⟨email#2⟩"), "got {red}");
        assert!(!red.contains('@'), "no raw address survives");
        assert_eq!(hits.iter().find(|h| h.rule == "email").unwrap().count, 2);
    }

    #[test]
    fn redacts_secret_keys_before_anything_else() {
        let (red, hits) = redactor().redact_text("key sk-ABCDEFGHIJKLMNOPQRSTUV live");
        assert!(red.contains("⟨secret#1⟩"), "got {red}");
        assert_eq!(hits.iter().find(|h| h.rule == "secret_key").unwrap().count, 1);
    }

    #[test]
    fn redacts_github_classic_and_fine_grained_pats() {
        // Classic PAT: prefix + 36 alnum. All five classic prefixes share the rule;
        // ghp_ and ghs_ stand in for the class.
        let text = "push with ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij and \
                    ghs_0123456789abcdefghijABCDEFGHIJ456789 done";
        let (red, hits) = redactor().redact_text(text);
        assert!(red.contains("⟨secret#1⟩") && red.contains("⟨secret#2⟩"), "got {red}");
        assert!(!red.contains("ghp_") && !red.contains("ghs_"), "no raw PAT survives: {red}");
        assert_eq!(hits.iter().find(|h| h.rule == "secret_key").unwrap().count, 2);
        // Fine-grained PAT: github_pat_ + 22 alnum + `_` + 59 alnum.
        let fg = "github_pat_11ABCDEFGHIJKLMNOPQRST_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456";
        let (red, hits) = redactor().redact_text(&format!("token {fg} end"));
        assert!(red.contains("⟨secret#1⟩") && !red.contains("github_pat_"), "got {red}");
        assert_eq!(hits.iter().find(|h| h.rule == "secret_key").unwrap().count, 1);
        // Negative: a short prose mention is not a token.
        let (red, hits) = redactor().redact_text("set ghp_test as a placeholder name");
        assert_eq!(red, "set ghp_test as a placeholder name");
        assert!(hits.is_empty());
    }

    #[test]
    fn redacts_slack_tokens() {
        // Assembled at runtime: a source-literal Slack-shaped token trips
        // GitHub push protection (it has no checksum to disprove realness).
        let tok = format!("xoxb-{}-{}-{}", "123456789012", "1234567890123", "AbCdEfGhIjKlMnOpQrStUvWx");
        let (red, hits) = redactor().redact_text(&format!("bot {tok}"));
        assert!(red.contains("⟨secret#1⟩") && !red.contains("xoxb-"), "got {red}");
        assert_eq!(hits.iter().find(|h| h.rule == "secret_key").unwrap().count, 1);
        // Negative: the bare prefix with nothing token-like after it is left alone.
        let (red, hits) = redactor().redact_text("the xoxb- prefix marks bot tokens");
        assert_eq!(red, "the xoxb- prefix marks bot tokens");
        assert!(hits.is_empty());
    }

    #[test]
    fn redacts_authorization_bearer_header_but_not_prose_bearer() {
        let (red, hits) =
            redactor().redact_text("curl -H 'Authorization: Bearer mF_9.B5f-4.1JqM0aXk8ZZZZ' https://x");
        assert!(red.contains("⟨secret#1⟩"), "got {red}");
        assert!(!red.contains("mF_9.B5f-4"), "no raw bearer token survives: {red}");
        assert_eq!(hits.iter().find(|h| h.rule == "secret_key").unwrap().count, 1);
        // Lowercase header form (as seen in raw HTTP dumps) is caught too.
        let (red, _) = redactor().redact_text("authorization: bearer abcdefghijklmnop0123");
        assert!(red.contains("⟨secret#1⟩"), "lowercase header redacted: {red}");
        // Negatives: ordinary prose with the word Bearer must not be nuked.
        for prose in [
            "the bearer of good news arrived unannounced",
            "Bearer tokens are described in RFC 6750 at length",
            "Authorization: Bearer auth",
        ] {
            let (red, hits) = redactor().redact_text(prose);
            assert_eq!(red, prose, "prose untouched");
            assert!(hits.is_empty());
        }
    }

    #[test]
    fn redacts_a_full_pem_private_key_body_as_one_hit() {
        let text = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA7x9z\nQWJjZGVmZ2hpamts\n-----END RSA PRIVATE KEY-----\nafter";
        let (red, hits) = redactor().redact_text(text);
        assert!(red.contains("⟨secret#1⟩"), "got {red}");
        assert!(
            !red.contains("MIIEpAIBAAKCAQEA7x9z") && !red.contains("QWJjZGVmZ2hpamts"),
            "no base64 body line survives: {red}"
        );
        assert!(red.starts_with("before\n") && red.ends_with("\nafter"), "context kept: {red}");
        // The whole body is ONE hit — the header-only fallback must not double-fire.
        assert_eq!(hits.iter().find(|h| h.rule == "secret_key").unwrap().count, 1);
    }

    #[test]
    fn redacts_an_openssh_key_body_and_keeps_two_keys_as_two_hits() {
        let text = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEA\n-----END OPENSSH PRIVATE KEY-----\nand\n-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIB\n-----END EC PRIVATE KEY-----";
        let (red, hits) = redactor().redact_text(text);
        assert!(red.contains("⟨secret#1⟩") && red.contains("⟨secret#2⟩"), "got {red}");
        assert!(
            !red.contains("b3BlbnNzaC1rZXktdjEA") && !red.contains("MHcCAQEEIB"),
            "no key material survives: {red}"
        );
        // Non-greedy body match: two keys are two hits, not one span across both.
        assert_eq!(hits.iter().find(|h| h.rule == "secret_key").unwrap().count, 2);
    }

    #[test]
    fn a_truncated_pem_header_without_an_end_line_still_redacts() {
        let (red, hits) = redactor().redact_text("saw -----BEGIN PRIVATE KEY----- then the capture cut off");
        assert!(red.contains("⟨secret#1⟩"), "got {red}");
        assert!(!red.contains("BEGIN PRIVATE KEY"), "header line scrubbed: {red}");
        assert_eq!(hits.iter().find(|h| h.rule == "secret_key").unwrap().count, 1);
    }

    #[test]
    fn only_luhn_valid_digit_runs_are_treated_as_cards() {
        let (red, hits) = redactor().redact_text("card 4111111111111111 order 1234567890123");
        assert!(red.contains("⟨card#1⟩"), "the valid card is redacted: {red}");
        assert!(red.contains("1234567890123"), "the non-Luhn order number is left alone");
        assert_eq!(hits.iter().find(|h| h.rule == "payment_card").unwrap().count, 1);
    }

    #[test]
    fn redacts_a_contiguous_iban() {
        let (red, hits) = redactor().redact_text("pay to DE89370400440532013000 today");
        assert!(red.contains("⟨iban#1⟩"), "got {red}");
        assert!(!red.contains("DE89370400440532013000"), "no raw IBAN survives");
        assert_eq!(hits.iter().find(|h| h.rule == "iban").unwrap().count, 1);
    }

    #[test]
    fn redacts_a_formatted_phone_but_not_a_bare_run() {
        // Positive: a separator-formatted number is a phone.
        let (red, hits) = redactor().redact_text("call me +1 (555) 123-4567 later");
        assert!(red.contains("⟨phone#1⟩"), "formatted phone redacted: {red}");
        assert_eq!(hits.iter().find(|h| h.rule == "phone").unwrap().count, 1);
        // Negative: a bare separator-free 10-digit run is left for the human.
        let (bare, bare_hits) = redactor().redact_text("ref 5551234567 ok");
        assert!(bare.contains("5551234567"), "a bare digit run is not a phone: {bare}");
        assert!(bare_hits.iter().all(|h| h.rule != "phone"));
    }

    #[test]
    fn redacts_numeric_pii_carried_as_a_json_number() {
        // A card number as a JSON integer must not escape redaction (review #15).
        let mut payload = ContextPayload {
            payload_id: Uuid::nil(),
            created_ts: 0,
            intent: Intent::SummarizeCurrent,
            items: vec![PayloadItem::Connector {
                connector_type: "browser".into(),
                payload: serde_json::json!({ "acct": 4111111111111111i64 }),
            }],
            redactions: vec![],
            enrichment_offered: false,
            transport_target: TransportTarget::MessagesApi,
            user_approved: false,
        };
        let hits = redactor().redact_payload(&mut payload);
        assert_eq!(hits.iter().find(|h| h.rule == "payment_card").map(|h| h.count), Some(1));
        let wire = serde_json::to_string(&payload.items[0]).unwrap();
        assert!(!wire.contains("4111111111111111"), "numeric card scrubbed: {wire}");
    }

    #[test]
    fn redact_payload_scrubs_event_trail_json_strings() {
        let mut payload = ContextPayload {
            payload_id: Uuid::nil(),
            created_ts: 0,
            intent: Intent::ExplainPattern,
            items: vec![PayloadItem::EventTrail {
                events: vec![serde_json::json!({ "note": "ping bob@corp.com re: sync" })],
            }],
            redactions: vec![],
            enrichment_offered: false,
            transport_target: TransportTarget::MessagesApi,
            user_approved: false,
        };
        let hits = redactor().redact_payload(&mut payload);
        assert_eq!(hits.iter().find(|h| h.rule == "email").map(|h| h.count), Some(1));
        match &payload.items[0] {
            PayloadItem::EventTrail { events } => {
                assert!(!events[0].to_string().contains("bob@corp.com"), "event-trail PII scrubbed");
            }
            _ => panic!("item 0 is the event trail"),
        }
    }

    #[test]
    fn clean_text_is_untouched_and_reports_nothing() {
        let (red, hits) = redactor().redact_text("just a normal sentence about rust");
        assert_eq!(red, "just a normal sentence about rust");
        assert!(hits.is_empty());
    }

    #[test]
    fn user_terms_redact_and_a_bad_regex_is_rejected() {
        let r = Redactor::new(&[UserTerm { pattern: "ProjectNimbus".into(), is_regex: false }]).unwrap();
        let (red, hits) = r.redact_text("ship ProjectNimbus by friday");
        assert!(red.contains("⟨term#1⟩"), "got {red}");
        assert_eq!(hits.iter().find(|h| h.rule == "user_defined").unwrap().count, 1);
        assert!(
            matches!(
                Redactor::new(&[UserTerm { pattern: "(".into(), is_regex: true }]),
                Err(PrivacyError::InvalidRule { .. })
            ),
            "an invalid user regex is rejected at construction"
        );
    }

    /// Derive, by diffing `input` against `redact_text`'s `output`, which input
    /// bytes were replaced by a placeholder: the literal pieces between
    /// placeholders must survive in order, and everything between them was
    /// redacted. (Inputs are chosen so no literal piece recurs inside a hit.)
    fn replaced_bytes(input: &str, output: &str) -> Vec<bool> {
        let ph = Regex::new(r"⟨[a-z]+#\d+⟩").unwrap();
        let pieces: Vec<&str> = ph.split(output).collect();
        let mut covered = vec![false; input.len()];
        assert!(input.starts_with(pieces[0]), "prefix survives: {input:?} -> {output:?}");
        let mut ip = pieces[0].len();
        for (k, piece) in pieces.iter().enumerate().skip(1) {
            let at = if k == pieces.len() - 1 {
                assert!(input.ends_with(piece), "suffix survives: {input:?} -> {output:?}");
                input.len() - piece.len()
            } else {
                ip + input[ip..].find(piece).expect("literal piece survives in order")
            };
            assert!(at >= ip);
            covered[ip..at].iter_mut().for_each(|b| *b = true);
            ip = at + piece.len();
        }
        covered
    }

    #[test]
    fn find_spans_covers_exactly_the_bytes_redact_text_replaces() {
        let r = Redactor::new(&[UserTerm { pattern: "ProjectNimbus".into(), is_regex: false }]).unwrap();
        let inputs = [
            "mail me at a@b.com or c@d.org",
            "key sk-ABCDEFGHIJKLMNOPQRSTUV live, card 4111 1111 1111 1111, call +1 (555) 123-4567 now",
            "pay DE89370400440532013000 to bob@corp.com re: ProjectNimbus",
            "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA7x9z\n-----END RSA PRIVATE KEY-----\nafter",
            // Gates: a non-Luhn run and a bare digit run are NOT hits for either path.
            "order 1234567890123 ref 5551234567 ok",
            "just a normal sentence about rust",
            "",
        ];
        for input in inputs {
            let (output, hits) = r.redact_text(input);
            let spans = r.find_spans(input);
            // Byte coverage is identical.
            let mut from_spans = vec![false; input.len()];
            for s in &spans {
                assert!(s.start < s.end && s.end <= input.len(), "well-formed span {s:?}");
                from_spans[s.start..s.end].iter_mut().for_each(|b| *b = true);
            }
            assert_eq!(from_spans, replaced_bytes(input, &output), "coverage for {input:?}");
            // Spans are sorted and disjoint.
            for w in spans.windows(2) {
                assert!(w[0].end <= w[1].start, "disjoint + sorted: {spans:?}");
            }
            // Per-rule attribution matches redact_text's counts.
            for kind in RuleKind::ORDERED {
                let n_spans = spans.iter().filter(|s| s.rule == kind).count() as u32;
                let n_hits = hits.iter().find(|h| h.rule == kind.label()).map_or(0, |h| h.count);
                assert_eq!(n_spans, n_hits, "{} count for {input:?}", kind.label());
            }
        }
    }

    #[test]
    fn find_spans_lets_earlier_rules_win_overlaps() {
        // The PEM body regex claims the whole key; the header-only fallback
        // (same rule, later pattern) and any later rule must not re-claim inside it.
        let text = "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIB 4111 1111 1111 1111\n-----END EC PRIVATE KEY-----";
        let spans = redactor().find_spans(text);
        assert_eq!(spans.len(), 1, "one span: {spans:?}");
        assert_eq!(spans[0], RedactionSpan { rule: RuleKind::SecretKey, start: 0, end: text.len() });
    }

    #[test]
    fn redact_payload_walks_items_flags_ocr_and_merges_counts() {
        let mut payload = ContextPayload {
            payload_id: Uuid::nil(),
            created_ts: 0,
            intent: Intent::SummarizeCurrent,
            items: vec![
                PayloadItem::OcrText {
                    source_event_id: 1,
                    text: "contact a@b.com".into(),
                    redacted: false,
                },
                PayloadItem::Connector {
                    connector_type: "browser".into(),
                    payload: serde_json::json!({ "note": "also c@d.com" }),
                },
                PayloadItem::UserAddition { text: "no pii here".into() },
            ],
            redactions: vec![],
            enrichment_offered: false,
            transport_target: TransportTarget::MessagesApi,
            user_approved: false,
        };
        let hits = redactor().redact_payload(&mut payload);
        // Two emails across an OcrText + a Connector JSON string.
        assert_eq!(hits.iter().find(|h| h.rule == "email").unwrap().count, 2);
        // The OcrText item is flagged + rewritten; the counts land on the payload.
        match &payload.items[0] {
            PayloadItem::OcrText { redacted, text, .. } => {
                assert!(*redacted && !text.contains('@'));
            }
            _ => panic!("item 0 is OcrText"),
        }
        // Counts are mirrored onto the payload (before preview).
        assert_eq!(payload.redactions.len(), hits.len());
        for (on_payload, returned) in payload.redactions.iter().zip(&hits) {
            assert_eq!(on_payload.rule, returned.rule);
            assert_eq!(on_payload.count, returned.count);
        }
    }
}
