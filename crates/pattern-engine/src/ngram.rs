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
    /// `patterns` row key (doc 08 §4). Uses [`Token::encode`], which is
    /// separator-safe, so the encoding round-trips stably and feedback
    /// (doc 08 §7) always hits the same row.
    pub fn signature(&self) -> String {
        let ant = self
            .antecedent
            .iter()
            .map(Token::encode)
            .collect::<Vec<_>>()
            .join(" | ");
        format!("{ant} ⇒ {}", self.consequent.encode())
    }

    /// The antecedent-only key (`join(antecedent) ⇒ *`) — the denominator row
    /// for confidence (doc 08 §4: `W(antecedent ⇒ *)`).
    pub fn antecedent_key(&self) -> String {
        let ant = self
            .antecedent
            .iter()
            .map(Token::encode)
            .collect::<Vec<_>>()
            .join(" | ");
        format!("{ant} ⇒ *")
    }
}

/// Parse a persisted `signature` back into the `(antecedent_key, consequent)`
/// pair a hydrate needs (CONN-M2) — the partial inverse of [`NGram::signature`].
/// The antecedent key (`"… ⇒ *"`) re-links the row into the sibling index for
/// candidate generation, and the decoded consequent token lets the hydrated row
/// form a candidate without waiting to be re-mined. Returns `None` for a
/// malformed signature (missing separator or an unparseable consequent), which
/// the caller skips rather than aborting the whole load.
pub fn parse_signature(signature: &str) -> Option<(String, Token)> {
    let (antecedent, consequent) = signature.split_once(" ⇒ ")?;
    let consequent = Token::decode(consequent)?;
    Some((format!("{antecedent} ⇒ *"), consequent))
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
    ///
    /// Consecutive duplicate tokens are collapsed (a focus storm on one window
    /// is one behavioral step, not four — doc 05 §4 debounce is upstream but
    /// heartbeat samples can still repeat a context).
    pub fn push(&mut self, tok: Token) -> Vec<NGram> {
        if self.tokens.last() == Some(&tok) {
            return Vec::new(); // duplicate step; nothing new closes
        }
        self.tokens.push(tok);
        if self.tokens.len() > MAX_N {
            let overflow = self.tokens.len() - MAX_N;
            self.tokens.drain(..overflow);
        }

        let len = self.tokens.len();
        let mut out = Vec::new();
        for n in MIN_N..=MAX_N.min(len) {
            let slice = &self.tokens[len - n..];
            out.push(NGram {
                antecedent: slice[..n - 1].to_vec(),
                consequent: slice[n - 1].clone(),
            });
        }
        out
    }

    /// The current token tail (up to `MAX_N - 1`) used to match pattern
    /// antecedents during candidate generation (doc 08 §5).
    pub fn antecedent_tail(&self) -> &[Token] {
        let len = self.tokens.len();
        let take = (MAX_N - 1).min(len);
        &self.tokens[len - take..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(a: &str) -> Token {
        Token {
            app_class: a.into(),
            action: "focus".into(),
            resource_class: None,
        }
    }

    #[test]
    fn closing_ngrams_grow_with_history() {
        let mut w = NGramWindow::new();
        assert!(w.push(tok("a")).is_empty(), "one token closes nothing");
        let g2 = w.push(tok("b"));
        assert_eq!(g2.len(), 1, "a→b closes one 2-gram");
        assert_eq!(g2[0].signature(), "a:focus:∅ ⇒ b:focus:∅");
        let g3 = w.push(tok("c"));
        assert_eq!(g3.len(), 2, "2-gram b→c and 3-gram a,b→c");
        let g4 = w.push(tok("d"));
        assert_eq!(g4.len(), 3, "n=2,3,4 all close");
        let g5 = w.push(tok("e"));
        assert_eq!(g5.len(), 3, "window trimmed to MAX_N; still n=2..4");
    }

    #[test]
    fn parse_signature_round_trips_the_consequent_and_antecedent_key() {
        let mut w = NGramWindow::new();
        w.push(tok("a"));
        w.push(tok("b"));
        let g = &w.push(tok("c"))[0]; // "a | b ⇒ c" (3-gram) or "b ⇒ c"
        let sig = g.signature();
        let (ant_key, consequent) = parse_signature(&sig).expect("well-formed signature parses");
        assert_eq!(ant_key, g.antecedent_key(), "antecedent key is reconstructed");
        assert_eq!(consequent, g.consequent, "consequent decodes back to the same token");
    }

    #[test]
    fn parse_signature_rejects_malformed_input() {
        assert!(parse_signature("no-arrow-here").is_none());
        assert!(parse_signature("a ⇒ too:many:colons:here").is_none());
    }

    #[test]
    fn session_reset_clears_history() {
        let mut w = NGramWindow::new();
        w.push(tok("a"));
        w.push(tok("b"));
        w.reset();
        assert!(w.push(tok("c")).is_empty(), "no cross-session grams (doc 08 §3)");
    }

    #[test]
    fn duplicate_steps_collapse() {
        let mut w = NGramWindow::new();
        w.push(tok("a"));
        assert!(w.push(tok("a")).is_empty(), "duplicate focus is one step");
        assert_eq!(w.antecedent_tail().len(), 1);
    }

    #[test]
    fn antecedent_tail_is_capped() {
        let mut w = NGramWindow::new();
        for name in ["a", "b", "c", "d", "e"] {
            w.push(tok(name));
        }
        assert_eq!(w.antecedent_tail().len(), MAX_N - 1);
        assert_eq!(w.antecedent_tail()[0], tok("c"));
    }
}
