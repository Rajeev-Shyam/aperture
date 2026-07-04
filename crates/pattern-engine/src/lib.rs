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

use std::collections::HashMap;

use aperture_contracts::connector::ConnectorState;
use aperture_contracts::event::Event;
use aperture_contracts::suggestions::SuggestionCandidate;

use crate::feedback::MuteState;
use crate::ngram::NGramWindow;
use crate::normalizer::Token;
use crate::scorer::PatternStats;
use crate::sessionizer::Sessionizer;
use crate::temporal::TemporalHistogram;
use crate::trigger::{TriggerGate, TriggerInput};

/// A user reaction routed back into the feedback loop (doc 08 §7).
///
/// Sourced from the `SuggestionClicked` / `SuggestionDismissed` events (doc 03 §2),
/// an internally-tracked expiry, and the explicit "useful?" thumbs (Q81);
/// maps to [`feedback::Signal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackEvent {
    /// The user clicked the bubble — reinforce (`decay × 1.25`, clamped at 1.0).
    Clicked,
    /// The user dismissed the bubble — ladder: ×0.8 / ×0.6 / mute at 3rd (ADR-033).
    Dismissed,
    /// The bubble was ignored until it expired — mild penalty (`decay × 0.9`).
    Expired,
    /// Explicit "useful?" 👍 (Q81) — strong reinforce.
    ThumbsUp,
    /// Explicit "useful?" 👎 (Q81) — strong penalty + ladder advance.
    ThumbsDown,
}

impl From<FeedbackEvent> for feedback::Signal {
    fn from(fb: FeedbackEvent) -> Self {
        match fb {
            FeedbackEvent::Clicked => feedback::Signal::Clicked,
            FeedbackEvent::Dismissed => feedback::Signal::Dismissed,
            FeedbackEvent::Expired => feedback::Signal::Expired,
            FeedbackEvent::ThumbsUp => feedback::Signal::ThumbsUp,
            FeedbackEvent::ThumbsDown => feedback::Signal::ThumbsDown,
        }
    }
}

/// One cached pattern row (mirrors the `patterns` table, doc 03 §3).
#[derive(Debug, Clone, Default)]
pub struct PatternRow {
    /// Stable row id once persisted; negative until the DB assigns one.
    pub pattern_id: i64,
    pub stats: PatternStats,
    pub mute: MuteState,
    /// The consequent token, kept so a candidate can be formed without
    /// re-parsing the signature.
    pub consequent: Option<Token>,
    /// Dirty ⇒ needs flushing to the `patterns` table.
    pub dirty: bool,
}

/// Everything the engine needs from the outside world for one event, supplied
/// by the caller (the shell / the M3 gate harness): connector lookup is the
/// doc 10 seam (real registry at M4; `contracts::fakes::FakeConnector`-backed
/// in the SC2 gate), and `now_ms` keeps the engine clock-free and testable.
pub struct EngineContext<'a> {
    /// Fresh, resumable state for a consequent token, if any (trigger rule 3).
    pub connector_lookup: &'a dyn Fn(&Token) -> Option<ConnectorState>,
    /// Now, epoch ms.
    pub now_ms: i64,
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
    /// signature → cached pattern row (doc 03 `patterns`).
    patterns: HashMap<String, PatternRow>,
    /// antecedent key (`… ⇒ *`) → signatures sharing that antecedent — lets one
    /// observation grow every sibling's `W(ant ⇒ *)` denominator (doc 08 §4).
    antecedent_index: HashMap<String, Vec<String>>,
    /// pattern_id → signature (feedback arrives keyed by row id, doc 08 §7).
    id_index: HashMap<i64, String>,
    /// Per-resource temporal histograms (doc 08 §4).
    temporal: HashMap<String, TemporalHistogram>,
    /// resource_class → last time it was foreground (novelty rule 6, ADR-033).
    last_focused: HashMap<String, i64>,
    /// The resource class currently foreground.
    foreground_resource: Option<String>,
    /// Synthetic id source for rows not yet persisted (negative; replaced by DB
    /// ids at flush via [`Self::mark_flushed`]).
    next_local_id: i64,
}

impl PatternEngine {
    /// Construct an empty engine.
    ///
    /// Starts with `capture_on = false` until the orchestrator reports state
    /// (invariant 3): silence beats noise on cold start (doc 08 §9).
    pub fn new() -> Self {
        Self {
            sessionizer: Sessionizer::new(),
            window: NGramWindow::new(),
            gate: TriggerGate::new(),
            capture_on: false,
            patterns: HashMap::new(),
            antecedent_index: HashMap::new(),
            id_index: HashMap::new(),
            temporal: HashMap::new(),
            last_focused: HashMap::new(),
            foreground_resource: None,
            next_local_id: -1,
        }
    }

    /// Reflect a capture-toggle change (invariant 3, trigger rule 7, doc 08 §6.7).
    /// When set `false`, no candidate can pass [`on_event`](Self::on_event).
    pub fn set_capture(&mut self, on: bool) {
        self.capture_on = on;
    }

    /// Ingest one event and return any candidates that pass all 7 trigger rules
    /// (doc 08 §2-§6). Pure with respect to the network: **never a cloud call**.
    pub fn on_event(&mut self, ev: &Event, ctx: &EngineContext<'_>) -> Vec<SuggestionCandidate> {
        // Invariant 3 / rule 7: capture OFF ⇒ observe nothing, emit nothing.
        if !self.capture_on {
            return Vec::new();
        }

        // 1. normalize (§2) — None for audit/excluded/no-process events.
        let Some(token) = normalizer::normalize(ev) else {
            return Vec::new();
        };

        // 2. sessionize (§3); reset the window on a session boundary.
        let prev_session = self.sessionizer.current();
        let session = self.sessionizer.assign(ev);
        if prev_session.is_some() && prev_session != Some(session) {
            self.window.reset();
        }

        // 3a. temporal mining (§4): a return visit to this resource. Weight 1 at
        // observation; histogram mass ages via the read-side prune, and the
        // TEMPORAL half-life governs periodic scoring (ADR-033).
        if let Some(res) = &token.resource_class {
            let hist = self
                .temporal
                .entry(res.clone())
                .or_insert_with(|| TemporalHistogram::new(res.clone()));
            hist.record_return(ev.ts, 1.0);
        }

        // 3b. n-gram mining (§4): credit every closing n-gram.
        let closing = self.window.push(token.clone());
        for gram in &closing {
            let sig = gram.signature();
            let ant_key = gram.antecedent_key();

            let is_new = !self.patterns.contains_key(&sig);
            if is_new {
                let id = self.next_local_id;
                self.next_local_id -= 1;
                self.patterns.insert(
                    sig.clone(),
                    PatternRow {
                        pattern_id: id,
                        stats: {
                            let mut s = PatternStats::new(ctx.now_ms);
                            s.dismiss_decay = 1.0;
                            s
                        },
                        mute: MuteState::default(),
                        consequent: Some(gram.consequent.clone()),
                        dirty: true,
                    },
                );
                self.id_index.insert(id, sig.clone());
            }
            let row = self.patterns.get_mut(&sig).expect("inserted above");
            row.stats.credit_occurrence(ctx.now_ms);
            row.dirty = true;

            // Grow the `⇒ *` denominator of every sibling with this antecedent.
            let siblings = self.antecedent_index.entry(ant_key).or_default();
            if !siblings.contains(&sig) {
                siblings.push(sig.clone());
            }
            for sibling in siblings.clone() {
                if sibling != sig {
                    if let Some(other) = self.patterns.get_mut(&sibling) {
                        other.stats.credit_antecedent_only(ctx.now_ms);
                        other.dirty = true;
                    }
                }
            }
        }

        // Novelty bookkeeping (rule 6) — AFTER matching state below uses the
        // *previous* focus times; stamp this token's resource as focused now,
        // and make it the new foreground.
        // (Ordering note: candidates are generated from the tail that *includes*
        // this token, predicting the NEXT step — so stamping now is correct: the
        // predicted consequent is a different resource by the self-suppression
        // check, and its own last-focus stamp is from its previous appearance.)
        if let Some(res) = &token.resource_class {
            self.last_focused.insert(res.clone(), ev.ts);
        }
        self.foreground_resource = token.resource_class.clone();

        // 4-5. candidate generation + scoring (§5) + gating (§6): match every
        // suffix of the current tail against pattern antecedents.
        let tail: Vec<Token> = self.window.antecedent_tail().to_vec();
        let mut out = Vec::new();
        for suffix_len in 1..=tail.len() {
            let ant = &tail[tail.len() - suffix_len..];
            let ant_key = format!(
                "{} ⇒ *",
                ant.iter().map(Token::encode).collect::<Vec<_>>().join(" | ")
            );
            let Some(sigs) = self.antecedent_index.get(&ant_key) else {
                continue;
            };
            for sig in sigs.clone() {
                let Some(row) = self.patterns.get(&sig) else { continue };
                let Some(consequent) = row.consequent.clone() else { continue };

                // Never re-suggest the token we just observed.
                if consequent == token {
                    continue;
                }
                if row.mute.is_muted(ctx.now_ms) {
                    continue; // muted signatures stay silent (doc 08 §7)
                }

                let stats_now = row.stats.decayed_to(ctx.now_ms);
                let conf = stats_now.confidence();

                // Rule 3 seam: a fresh, resumable connector state (doc 10 / M4;
                // fakes in the M3 gate).
                let state = (ctx.connector_lookup)(&consequent);
                let fresh = state
                    .as_ref()
                    .map(|s| scorer::freshness(s, ctx.now_ms))
                    .unwrap_or(0.0);

                let cons_res = consequent.resource_class.as_deref();
                let nov = scorer::novelty(
                    cons_res,
                    self.foreground_resource.as_deref(),
                    cons_res.and_then(|r| self.last_focused.get(r).copied()),
                    ctx.now_ms,
                );
                let score = scorer::score(conf, stats_now.dismiss_decay, fresh, nov);

                let input = TriggerInput {
                    score,
                    weighted_support: stats_now.weighted_support,
                    connector_state: state.as_ref(),
                    signature: &sig,
                    dismissal_step: row.mute.dismissal_step(ctx.now_ms),
                    consequent_is_foreground: cons_res.is_some()
                        && cons_res == self.foreground_resource.as_deref(),
                    consequent_last_focused_ms: cons_res
                        .and_then(|r| self.last_focused.get(r).copied()),
                    now_ms: ctx.now_ms,
                };

                if self.gate.admit(&input, self.capture_on).is_ok() {
                    let state = state.expect("rule 3 held");
                    out.push(SuggestionCandidate {
                        action_template: action_template_for(&consequent),
                        connector_id: state.id.clone(),
                        confidence: score,
                        pattern_id: row.pattern_id,
                    });
                    self.gate.note_emitted(&sig, ctx.now_ms);
                }
            }
        }

        // Overflow rule (§6.5): keep the highest-score candidates first (the
        // downstream queue drops lowest on overflow).
        out.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        out
    }

    /// Route a user reaction for `pattern_id` back into the feedback loop
    /// (doc 08 §7): adjusts `dismiss_decay`, may mute the signature, and (for
    /// clicks/dismissals) nudges the adaptive cap (ADR-032).
    pub fn apply_feedback(&mut self, pattern_id: i64, fb: FeedbackEvent, now_ms: i64) {
        let Some(sig) = self.id_index.get(&pattern_id).cloned() else {
            return;
        };
        if let Some(row) = self.patterns.get_mut(&sig) {
            feedback::apply(&mut row.stats, &mut row.mute, fb.into(), now_ms);
            row.dirty = true;
        }
        match fb {
            FeedbackEvent::Clicked | FeedbackEvent::ThumbsUp => self.gate.adapt_cap(true),
            FeedbackEvent::Dismissed | FeedbackEvent::ThumbsDown => self.gate.adapt_cap(false),
            FeedbackEvent::Expired => {}
        }
    }

    /// Weekly maintenance hook (doc 08 §9): prune signatures with weighted
    /// support below [`config::PRUNE_SUPPORT_FLOOR`]. Scheduled by the
    /// orchestrator (doc 12). Returns rows pruned.
    pub fn prune(&mut self, now_ms: i64) -> usize {
        let mut flat: HashMap<String, (PatternStats, MuteState)> = self
            .patterns
            .iter()
            .map(|(k, v)| (k.clone(), (v.stats.clone(), v.mute.clone())))
            .collect();
        let doomed = feedback::prune_stale_patterns(&mut flat, now_ms);
        for sig in &doomed {
            if let Some(row) = self.patterns.remove(sig) {
                self.id_index.remove(&row.pattern_id);
            }
            for sigs in self.antecedent_index.values_mut() {
                sigs.retain(|s| s != sig);
            }
        }
        doomed.len()
    }

    /// Rows needing persistence (doc 03 `patterns`); the shell flushes these via
    /// `aperture_db` and calls [`Self::mark_flushed`] with the assigned ids.
    pub fn dirty_rows(&self) -> Vec<(&str, &PatternRow)> {
        self.patterns
            .iter()
            .filter(|(_, r)| r.dirty)
            .map(|(s, r)| (s.as_str(), r))
            .collect()
    }

    /// Record the DB-assigned id for a flushed row and clear its dirty bit.
    pub fn mark_flushed(&mut self, signature: &str, db_id: i64) {
        if let Some(row) = self.patterns.get_mut(signature) {
            self.id_index.remove(&row.pattern_id);
            row.pattern_id = db_id;
            row.dirty = false;
            self.id_index.insert(db_id, signature.to_string());
        }
    }

    /// Number of cached pattern rows (diagnostics / gate telemetry).
    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }
}

impl Default for PatternEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Render the default action template for a consequent token (doc 08 §6 →
/// suggestion-generator). The generator expands `{title}`/`{position}` from the
/// connector's `reconstruct_payload` (doc 08 §6, doc 11 §3).
fn action_template_for(consequent: &Token) -> String {
    match consequent.resource_class.as_deref() {
        Some("youtube") => "Continue {title} — {position}".to_string(),
        Some(r) if r.starts_with("doc:") => "Reopen {title}".to_string(),
        Some(r) if r.starts_with("ide:") => "Back to {title}:{line}".to_string(),
        Some(r) if r.starts_with("url:") => "Return to {title}".to_string(),
        _ => "Resume {title}".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aperture_contracts::EventType;

    const MIN: i64 = 60_000;

    fn nav_event(ts: i64, process: &str, url: &str) -> Event {
        Event {
            id: 0,
            ts,
            r#type: EventType::Navigation,
            app: None,
            process: Some(process.into()),
            window_title: None,
            payload: serde_json::json!({ "url": url }),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    fn focus_event(ts: i64, process: &str) -> Event {
        Event {
            id: 0,
            ts,
            r#type: EventType::WindowFocus,
            app: None,
            process: Some(process.into()),
            window_title: None,
            payload: serde_json::json!({}),
            connector_id: None,
            session_id: None,
            redaction_flags: 0,
        }
    }

    fn youtube_state() -> ConnectorState {
        ConnectorState {
            id: "conn-yt".into(),
            connector_type: "youtube".into(),
            reconstruct_payload: serde_json::json!({"video_id": "abc", "position_s": 754}),
            payload_version: 1,
            captured_ts: 0,
            stale_after_ts: None,
        }
    }

    /// The SC2-shaped script (doc 16 M3): open app A → do thing → open app B →
    /// repeat 3×; the third repetition must produce a candidate.
    #[test]
    fn recurring_workflow_produces_a_candidate_on_the_third_repetition() {
        let mut engine = PatternEngine::new();
        engine.set_capture(true);

        let lookup = |tok: &Token| -> Option<ConnectorState> {
            (tok.resource_class.as_deref() == Some("youtube")).then(youtube_state)
        };

        let mut ts = 0i64;
        for _rep in 0..3 {
            ts += 10 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            engine.on_event(&focus_event(ts, "code.exe"), &ctx);
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            engine.on_event(&nav_event(ts, "chrome.exe", "https://youtube.com/watch?v=abc"), &ctx);
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            engine.on_event(&focus_event(ts, "slack.exe"), &ctx);
        }

        // The 3rd repetition is complete (support ≥ 3, US1 acceptance (a)); the
        // next occurrence of the antecedent must produce the bubble (SC2).
        ts += 12 * MIN;
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
        let candidates = engine.on_event(&focus_event(ts, "code.exe"), &ctx);

        assert!(
            !candidates.is_empty(),
            "after 3 observed (ide → youtube) returns, the antecedent must trigger (SC2)"
        );
        let c = &candidates[0];
        assert_eq!(c.connector_id, "conn-yt");
        assert!(c.action_template.contains("{position}"), "youtube template");
        assert!(c.confidence >= config::TAU_CONF);
    }

    #[test]
    fn capture_off_emits_nothing_and_mines_nothing() {
        let mut engine = PatternEngine::new();
        engine.set_capture(false);
        let lookup = |_: &Token| -> Option<ConnectorState> { Some(youtube_state()) };
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: 0 };
        let got = engine.on_event(&focus_event(0, "code.exe"), &ctx);
        assert!(got.is_empty());
        assert_eq!(engine.pattern_count(), 0, "OFF ⇒ not even mining (invariant 3)");
    }

    #[test]
    fn no_fresh_connector_state_means_no_bubble() {
        let mut engine = PatternEngine::new();
        engine.set_capture(true);
        let lookup = |_: &Token| -> Option<ConnectorState> { None };
        let mut ts = 0;
        let mut all = Vec::new();
        for _ in 0..5 {
            ts += 10 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            all.extend(engine.on_event(&focus_event(ts, "code.exe"), &ctx));
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            all.extend(engine.on_event(
                &nav_event(ts, "chrome.exe", "https://youtube.com/watch?v=x"),
                &ctx,
            ));
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            all.extend(engine.on_event(&focus_event(ts, "slack.exe"), &ctx));
        }
        assert!(all.is_empty(), "rule 3: no fresh resumable state ⇒ silence");
    }

    #[test]
    fn feedback_mutes_a_dismissed_pattern() {
        let mut engine = PatternEngine::new();
        engine.set_capture(true);
        let lookup = |tok: &Token| -> Option<ConnectorState> {
            (tok.resource_class.as_deref() == Some("youtube")).then(youtube_state)
        };

        let mut pattern_id = None;
        let mut ts = 0;
        for _ in 0..4 {
            ts += 12 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            let got = engine.on_event(&focus_event(ts, "code.exe"), &ctx);
            if let Some(c) = got.first() {
                pattern_id = Some(c.pattern_id);
            }
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            engine.on_event(&nav_event(ts, "chrome.exe", "https://youtube.com/watch?v=a"), &ctx);
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            engine.on_event(&focus_event(ts, "slack.exe"), &ctx);
        }
        let id = pattern_id.expect("pattern fired at least once (4th antecedent, support 3)");

        engine.apply_feedback(id, FeedbackEvent::Dismissed, ts);
        engine.apply_feedback(id, FeedbackEvent::Dismissed, ts + MIN);
        engine.apply_feedback(id, FeedbackEvent::Dismissed, ts + 2 * MIN);

        ts += 40 * MIN;
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
        let got = engine.on_event(&focus_event(ts, "code.exe"), &ctx);
        assert!(
            got.iter().all(|c| c.pattern_id != id),
            "muted signature must not re-fire (doc 08 §7)"
        );
    }
}
