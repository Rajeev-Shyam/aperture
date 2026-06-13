//! Behavior & Pattern Engine (doc 08) — CPU-only, zero-cloud.
//!
//! Turns the event stream into proactive [`SuggestionCandidate`]s through a
//! fixed pipeline: normalize (§2) → sessionize (§3) → mine n-grams + temporal
//! patterns (§4) → score (§5) → gate against the 7 trigger rules (§6); a
//! feedback loop (§7) tunes it over time. The engine's output goes to the
//! Suggestion Generator → Bubble UI; **it never makes a cloud call** (doc 08 §1,
//! locked answer A) — only the reasoning-gateway crate may open sockets / spawn
//! the Claude CLI (invariant 2, the transparency gate). When capture is OFF the
//! engine emits nothing (invariant 3, the capture toggle; see [`trigger`]).
//!
//! Cost: CPU-only, incremental, `O(recent-window)` per event; negligible RAM
//! beyond the pattern-table cache (doc 08 §1).

pub mod config;
pub mod feedback;
pub mod ngram;
pub mod normalizer;
pub mod scorer;
pub mod sessionizer;
pub mod temporal;
pub mod trigger;

use aperture_contracts::event::Event;
use aperture_contracts::suggestions::SuggestionCandidate;

use crate::ngram::NGramWindow;
use crate::sessionizer::Sessionizer;
use crate::trigger::TriggerGate;

/// A user reaction routed back into the feedback loop (doc 08 §7).
///
/// Sourced from the `SuggestionClicked` / `SuggestionDismissed` events (doc 03 §2)
/// plus an internally-tracked expiry; maps to [`feedback::Signal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackEvent {
    /// The user clicked the bubble — reinforce (`decay × 1.25`, clamped at 1.0).
    Clicked,
    /// The user dismissed the bubble — penalize (`decay × 0.5`); two in 24 h mute it 7 d.
    Dismissed,
    /// The bubble was ignored until it expired — mild penalty (`decay × 0.9`).
    Expired,
}

impl From<FeedbackEvent> for feedback::Signal {
    fn from(fb: FeedbackEvent) -> Self {
        match fb {
            FeedbackEvent::Clicked => feedback::Signal::Clicked,
            FeedbackEvent::Dismissed => feedback::Signal::Dismissed,
            FeedbackEvent::Expired => feedback::Signal::Expired,
        }
    }
}

/// The stateful, single-threaded engine. One instance reads the event stream
/// incrementally (doc 08 §1) and owns the session/window/trigger state plus the
/// in-memory pattern-table cache (persisted via `aperture_db`, doc 03 / §4).
///
/// Not `Sync`-bound by contract: drive it from one task that consumes the bus.
pub struct PatternEngine {
    sessionizer: Sessionizer,
    window: NGramWindow,
    gate: TriggerGate,
    /// Whether capture is ON (trigger rule 7 / invariant 3). The orchestrator
    /// flips this; OFF ⇒ [`on_event`](Self::on_event) yields no candidates.
    capture_on: bool,
    // TODO(M3): pattern-table cache (signature → PatternStats + MuteState +
    // centroid) and per-resource temporal histograms, hydrated from / flushed to
    // aperture_db (doc 03 `patterns`); semantic-assist embeddings via
    // aperture_embedding (doc 08 §5).
}

impl PatternEngine {
    /// Construct an engine, hydrating pattern stats from the DB (doc 08 §4).
    ///
    /// Starts with `capture_on = false` until the orchestrator reports state
    /// (invariant 3): silence beats noise on cold start (doc 08 §9).
    pub fn new() -> Self {
        // TODO(M3): load patterns/temporal histograms from aperture_db; wire the
        // embedding handle for the semantic assist (doc 08 §4-§5).
        Self {
            sessionizer: Sessionizer::new(),
            window: NGramWindow::new(),
            gate: TriggerGate::new(),
            capture_on: false,
        }
    }

    /// Reflect a capture-toggle change (invariant 3, trigger rule 7, doc 08 §6.7).
    /// When set `false`, no candidate can pass [`on_event`](Self::on_event).
    pub fn set_capture(&mut self, on: bool) {
        self.capture_on = on;
    }

    /// Ingest one event and return any candidates that pass all 7 trigger rules
    /// (doc 08 §2-§6). Pure with respect to the network: **never a cloud call**.
    ///
    /// Pipeline: normalize (§2) → assign session (§3) → push to the n-gram window
    /// and credit closing n-grams + temporal returns with recency weight (§4) →
    /// match the current tail against pattern antecedents and score (§5) → admit
    /// via [`trigger::TriggerGate`] (§6). Returns `Vec` so a single event may
    /// surface multiple consequents (the queue drops lowest-score on overflow,
    /// doc 08 §6.5).
    pub fn on_event(&mut self, _ev: &Event) -> Vec<SuggestionCandidate> {
        // Invariant 3 / rule 7: capture OFF ⇒ emit nothing.
        if !self.capture_on {
            return Vec::new();
        }
        // TODO(M3): full pipeline —
        //   1. normalizer::normalize(ev)            (skip None / audit / excluded)
        //   2. sessionizer.assign(ev); reset window on a new session
        //   3. window.push(token) → credit each closing n-gram into PatternStats
        //      with scorer::recency_weight; temporal.record_return for the resource
        //   4. for each pattern whose antecedent matches window.antecedent_tail()
        //      (exact, or scorer::semantic_substitutes for one token):
        //        - look up a fresh, resumable connector_state (doc 10)
        //        - score = scorer::score(conf, dismiss_decay, freshness, novelty)
        //        - skip if feedback::MuteState::is_muted
        //   5. gate.admit(TriggerInput, self.capture_on); on Ok, gate.note_emitted
        //      and emit SuggestionCandidate{ action_template, connector_id,
        //      confidence, pattern_id } (action_template e.g. "Continue {title} — {position}")
        let _ = (&mut self.sessionizer, &mut self.window, &mut self.gate);
        todo!("M3: normalize→sessionize→mine→score→gate pipeline (doc 08 §2-§6)")
    }

    /// Route a user reaction for `pattern_id` back into the feedback loop
    /// (doc 08 §7): adjusts `dismiss_decay` and may mute the signature, then
    /// persists to the `patterns` table.
    pub fn apply_feedback(&mut self, _pattern_id: i64, _fb: FeedbackEvent) {
        // TODO(M3): look up the signature's PatternStats + MuteState by pattern_id,
        // call feedback::apply(.., fb.into(), now_ms), persist via aperture_db.
        todo!("M3: feedback::apply for pattern_id + persist (doc 08 §7)")
    }

    /// Weekly maintenance hook (doc 08 §9): prune signatures with weighted
    /// support below [`config::PRUNE_SUPPORT_FLOOR`]. Scheduled by the
    /// orchestrator (doc 12). Returns rows pruned.
    pub fn prune(&mut self) -> usize {
        // TODO(M3): delegate to feedback::prune_stale_patterns over this engine's
        // table (doc 08 §9).
        feedback::prune_stale_patterns()
    }
}

impl Default for PatternEngine {
    fn default() -> Self {
        Self::new()
    }
}
