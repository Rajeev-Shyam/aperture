//! Sliding n-gram signatures (doc 08 §4).
//!
//! Within a single session we extract sliding **n-grams (n = 2..4)** of
//! [`Token`]s. The trailing token is the *consequent*; the prefix is the
//! *antecedent*. The stored `signature = join(antecedent) ⇒ consequent` keys a
//! row in the `patterns` table (doc 03 / doc 08 §4). Statistics (support,
//! confidence) live in [`crate::scorer`].

use crate::normalizer::Token;

/// Inclusive n-gram order range mined per session (doc 08 §4).
pub const MIN_N: usize = 2;
/// Upper bound (inclusive) of the n-gram order (doc 08 §4).
pub const MAX_N: usize = 4;

/// An extracted n-gram split into its antecedent prefix and consequent tail
/// (doc 08 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NGram {
    /// The prefix tokens (length `n - 1`).
    pub antecedent: Vec<Token>,
    /// The trailing token being predicted.
    pub consequent: Token,
}

impl NGram {
    /// The stable `signature` string `join(antecedent) ⇒ consequent` used as the
    /// `patterns` row key (doc 08 §4).
    pub fn signature(&self) -> String {
        // TODO(M3): canonical, collision-free encoding of (app_class, action,
        // resource_class) tuples joined by a separator, then " ⇒ " + consequent.
        // Must round-trip stably so feedback (doc 08 §7) hits the same row.
        todo!("M3: encode antecedent ⇒ consequent signature (doc 08 §4)")
    }
}

/// A session-local ring of recent tokens; produces the current matching tail and
/// all closing n-grams as each new token arrives (doc 08 §4-§5).
#[derive(Debug, Default)]
pub struct NGramWindow {
    /// Tokens for the current session in arrival order; trimmed to `MAX_N`.
    tokens: Vec<Token>,
}

impl NGramWindow {
    /// Empty window (call [`reset`](Self::reset) on a new session).
    pub fn new() -> Self {
        Self::default()
    }

    /// Clear the buffer at a session boundary so sequences never cross sessions
    /// (doc 08 §3-§4).
    pub fn reset(&mut self) {
        self.tokens.clear();
    }

    /// Push the newest token and return every n-gram (n = 2..4) that *ends* on
    /// it — i.e. the occurrences to credit in this step (doc 08 §4).
    pub fn push(&mut self, _tok: Token) -> Vec<NGram> {
        // TODO(M3): append; keep only the last MAX_N tokens; for n in MIN_N..=MAX_N
        // where enough history exists, emit NGram{ antecedent: prefix, consequent }.
        todo!("M3: emit closing n-grams for n=2..4 within the session (doc 08 §4)")
    }

    /// The current token tail (up to `MAX_N - 1`) used to match pattern
    /// antecedents during candidate generation (doc 08 §5).
    pub fn antecedent_tail(&self) -> &[Token] {
        // TODO(M3): return the trailing up-to-(MAX_N-1) tokens.
        &self.tokens
    }
}
