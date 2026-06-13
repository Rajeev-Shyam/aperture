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
//! | 1     | `SecretKey`     | regex: AWS keys, `sk-…`, PEM headers, JWT          |
//! | 2     | `PaymentCard`   | 13–19 digit runs passing the Luhn check            |
//! | 3     | `Iban`          | country-prefixed IBAN regex                        |
//! | 4     | `Email`         | RFC-lite regex                                     |
//! | 5     | `Phone`         | E.164-ish + common local formats                   |
//! | 6     | `UserDefined`   | literal/regex terms from settings                  |
//!
//! Misses are mitigated by the preview's per-item remove/edit affordance — the
//! human is the last redactor by design (doc 13 §5, §9).

use aperture_contracts::context_payload::{ContextPayload, PayloadItem, Redaction};

use crate::PrivacyError;

/// The fixed taxonomy of redaction rule kinds (doc 13 §5). Ordering of the
/// variants matches the pipeline order; [`RuleKind::ORDERED`] is the source of
/// truth for execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// 1 — secrets/keys: AWS access keys, `sk-…` tokens, PEM headers, JWTs.
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

/// A user-defined redaction term (doc 13 §5 rule 6, from settings).
#[derive(Debug, Clone)]
pub struct UserTerm {
    /// The literal string or regex source.
    pub pattern: String,
    /// `true` => `pattern` is a regex; `false` => match literally.
    pub is_regex: bool,
}

/// The compiled rule set. Built-in regexes are compiled once; user terms are
/// compiled from settings. Compilation can fail only for invalid user regex
/// ([`PrivacyError::InvalidRule`]).
pub struct Redactor {
    // TODO(M9): compiled regexes for rules 1–5 + the user-term matchers.
    // secret: Vec<regex::Regex>,
    // iban: regex::Regex,
    // email: regex::Regex,
    // phone: regex::Regex,
    // user_terms: Vec<(RuleKind, regex::Regex)>,
}

impl Redactor {
    /// Build the redactor with the built-in rules plus the user's configured
    /// terms (doc 13 §5 rule 6). Returns [`PrivacyError::InvalidRule`] if a
    /// user-supplied regex does not compile.
    pub fn new(_user_terms: &[UserTerm]) -> Result<Self, PrivacyError> {
        // TODO(M9): compile AWS/`sk-`/PEM/JWT secret patterns, IBAN, email,
        // phone; compile each UserTerm (escaping literals when `!is_regex`).
        todo!("M9: compile ordered redaction rule set (doc 13 §5)")
    }

    /// Redact a payload in place: run [`RuleKind::ORDERED`] over every text-bearing
    /// item, replacing hits with typed placeholders and accumulating the per-rule
    /// counts into `payload.redactions`. Runs at assembly, **before** preview.
    ///
    /// `OcrText` items whose text changed are flagged `redacted = true`. Returns
    /// the redactions for convenience (also written onto the payload).
    ///
    /// INVARIANT (2): this mutates an in-process object only; nothing here egresses.
    pub fn redact_payload(&self, _payload: &mut ContextPayload) -> Vec<Redaction> {
        // TODO(M9):
        //   for each text-bearing item (OcrText.text, UserAddition.text, and the
        //   stringly fields of EventTrail/Connector values):
        //     apply rules in RuleKind::ORDERED, replacing with ⟨noun#n⟩ placeholders
        //     (n is per-(item, kind) 1-based; counter via redact_text);
        //   merge per-kind counts into payload.redactions (additive on the Vec);
        //   set OcrText.redacted = true where text changed.
        todo!("M9: ordered in-place payload redaction -> Vec<Redaction> (doc 13 §5)")
    }

    /// Redact a single string, returning the rewritten text plus the per-rule hit
    /// counts. The numbering of `⟨noun#n⟩` placeholders is 1-based per rule kind
    /// within this call. Used by [`Redactor::redact_payload`] and unit tests.
    pub fn redact_text(&self, _input: &str) -> (String, Vec<Redaction>) {
        // TODO(M9): apply ordered rules, build placeholders, count hits.
        todo!("M9: single-string ordered redaction (doc 13 §5)")
    }
}

/// Luhn checksum used by rule 2 (payment cards, doc 13 §5). A 13–19 digit run is
/// only redacted when it passes Luhn — this keeps long ID/order numbers from
/// being falsely scrubbed.
pub fn luhn_valid(_digits: &str) -> bool {
    // TODO(M9): standard Luhn over ASCII digits only; reject if len not 13..=19.
    todo!("M9: Luhn check (doc 13 §5 rule 2)")
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
