//! Behavior & Pattern Engine (doc 08) — CPU-only, zero-cloud.
//!
//! Turns the event stream into proactive [`SuggestionCandidate`]s through a
//! fixed pipeline: normalize (§2) → sessionize (§3) → mine n-grams + temporal
//! patterns (§4) → score (§5) → gate against the 7 trigger rules (§6); a
//! feedback loop (§7) tunes it over time. Both pattern kinds can fire: temporal
//! (time-of-day) patterns trigger through the same gate as sequences (owner
//! decision #16, 2026-08-16), and pure app-focus consequents produce the
//! lighter "switch to X" candidate instead of staying silent (decision #15).
//! Runtime tunables load from the `pattern_engine` settings block via
//! [`PatternEngine::set_config`] (decision #17). The engine's output goes to the
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
    /// Explicit "Mute this pattern" (doc 11 §3) — straight to the 7-day mute.
    Muted,
}

impl From<FeedbackEvent> for feedback::Signal {
    fn from(fb: FeedbackEvent) -> Self {
        match fb {
            FeedbackEvent::Clicked => feedback::Signal::Clicked,
            FeedbackEvent::Dismissed => feedback::Signal::Dismissed,
            FeedbackEvent::Expired => feedback::Signal::Expired,
            FeedbackEvent::ThumbsUp => feedback::Signal::ThumbsUp,
            FeedbackEvent::ThumbsDown => feedback::Signal::ThumbsDown,
            FeedbackEvent::Muted => feedback::Signal::Muted,
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

/// One persisted `patterns` row (doc 03 §3) as read from the DB, fed to
/// [`PatternEngine::hydrate`] at startup (CONN-M2). Mirrors the flushed columns
/// plus the mute-ladder state (`muted_until` / `recent_dismissals`, persisted by
/// migration 0002) so the decay/mute ladder survives a restart.
#[derive(Debug, Clone)]
pub struct PersistedPattern {
    /// The DB-assigned row id (feedback routes by it — seeds `id_index`).
    pub pattern_id: i64,
    /// The n-gram signature key (`"… ⇒ c"`), parsed back for the sibling index.
    pub signature: String,
    /// `round(weighted_support)` as flushed — the reconstructed weighted support.
    pub support: i64,
    /// `W(ant⇒cons)/W(ant⇒*)` as flushed — reconstructs `antecedent_total`.
    pub confidence: f64,
    /// `last_updated_ms` the stats were valid at (the decay re-base point).
    pub last_seen: i64,
    /// The feedback multiplier — the load-bearing suppression datum (doc 08 §7).
    pub dismiss_decay: f64,
    /// Muted-until epoch ms, if the signature was muted (3rd dismissal, ADR-033).
    pub muted_until: Option<i64>,
    /// Dismissal timestamps still in the trailing trip-wire window (ladder step).
    pub recent_dismissals: Vec<i64>,
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
    /// The runtime tunables (decision #17) — settings-loaded by the shell via
    /// [`Self::set_config`]; compile-time constants until then.
    config: config::EngineConfig,
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
    /// app_class → last time it was foreground — the APP-level novelty ledger
    /// for "switch to X" candidates (decision #15), which have no resource class.
    last_focused_app: HashMap<String, i64>,
    /// The app class currently foreground (decision #15).
    foreground_app: Option<String>,
    /// app_class → the most recently observed raw process name — the honest
    /// launch target for a "switch to X" action (decision #15). In-memory only:
    /// a class not observed since startup simply produces no switch bubble.
    last_process: HashMap<String, String>,
    /// Synthetic id source for rows not yet persisted (negative; replaced by DB
    /// ids at flush via [`Self::mark_flushed`]).
    next_local_id: i64,
    /// The session id assigned to the most recent [`Self::on_event`] event —
    /// `None` when that event was not sessionized (capture off / not minable).
    /// The shell stamps it back onto the durable events row (doc 03 §3).
    last_session: Option<i64>,
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
            config: config::EngineConfig::default(),
            capture_on: false,
            patterns: HashMap::new(),
            antecedent_index: HashMap::new(),
            id_index: HashMap::new(),
            temporal: HashMap::new(),
            last_focused: HashMap::new(),
            foreground_resource: None,
            last_focused_app: HashMap::new(),
            foreground_app: None,
            last_process: HashMap::new(),
            next_local_id: -1,
            last_session: None,
        }
    }

    /// Construct with the session id source hydrated past the DB's max
    /// (doc 03 §3: `session_id` is monotonic across restarts — ADR-032 forbids
    /// retro-sessionizing, so ids must never collide with persisted rows).
    pub fn with_next_session_id(next_id: i64) -> Self {
        Self {
            sessionizer: Sessionizer::with_next_id(next_id),
            ..Self::new()
        }
    }

    /// The session id assigned to the most recent [`Self::on_event`] event
    /// (doc 08 §3), or `None` when that event was not sessionized. The shell
    /// stamps it onto the durable events row — SQLite is the durable truth
    /// (doc 15 §1); in-memory sessions alone would leave every row NULL.
    pub fn last_session(&self) -> Option<i64> {
        self.last_session
    }

    /// Reflect a capture-toggle change (invariant 3, trigger rule 7, doc 08 §6.7).
    /// When set `false`, no candidate can pass [`on_event`](Self::on_event).
    pub fn set_capture(&mut self, on: bool) {
        self.capture_on = on;
    }

    /// Apply the `pattern_engine` settings block (owner decision #17,
    /// 2026-08-16): threads the runtime tunables into the trigger gate and the
    /// sessionizer's cold-start gap, and stores the half-lives the scoring path
    /// reads. Safe to call again on a settings re-read — the gate keeps an
    /// adaptively-earned cap (re-clamped to the new band) rather than resetting.
    pub fn set_config(&mut self, config: config::EngineConfig) {
        self.gate.configure(&config);
        self.sessionizer
            .set_cold_start_gap_min(config.session_gap_cold_start_min);
        self.config = config;
    }

    /// Ingest one event and return any candidates that pass all 7 trigger rules
    /// (doc 08 §2-§6). Pure with respect to the network: **never a cloud call**.
    pub fn on_event(&mut self, ev: &Event, ctx: &EngineContext<'_>) -> Vec<SuggestionCandidate> {
        self.last_session = None; // set only if this event sessionizes below
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
        self.last_session = Some(session);
        if prev_session.is_some() && prev_session != Some(session) {
            self.window.reset();
        }

        // 3a. temporal mining (§4): a return visit to this resource. Existing
        // mass ages to `ev.ts` with the TEMPORAL half-life (ADR-033, tunable —
        // decision #17), then the visit adds weight 1.
        if let Some(res) = &token.resource_class {
            let hist = self
                .temporal
                .entry(res.clone())
                .or_insert_with(|| TemporalHistogram::new(res.clone()));
            hist.record_return(ev.ts, self.config.half_life_temporal_days);
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
            let half_life = self.config.half_life_sequence_days;
            let row = self.patterns.get_mut(&sig).expect("inserted above");
            row.stats.credit_occurrence(ctx.now_ms, half_life);
            row.dirty = true;

            // Grow the `⇒ *` denominator of every sibling with this antecedent.
            let siblings = self.antecedent_index.entry(ant_key).or_default();
            if !siblings.contains(&sig) {
                siblings.push(sig.clone());
            }
            for sibling in siblings.clone() {
                if sibling != sig {
                    if let Some(other) = self.patterns.get_mut(&sibling) {
                        other.stats.credit_antecedent_only(ctx.now_ms, half_life);
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
        // App-level novelty ledger for "switch to X" candidates (decision #15):
        // every token carries an app_class; also remember the class's concrete
        // process name so the switch action has a real launch target.
        self.last_focused_app.insert(token.app_class.clone(), ev.ts);
        self.foreground_app = Some(token.app_class.clone());
        if let Some(p) = ev.process.as_deref() {
            self.last_process.insert(token.app_class.clone(), p.to_string());
        }

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

                let stats_now = row
                    .stats
                    .decayed_to(ctx.now_ms, self.config.half_life_sequence_days);
                let conf = stats_now.confidence();

                // Decision #15 (2026-08-16): a pure app-focus consequent (window
                // focus/open, no resource class) has no resumable state by
                // design — it takes the lighter "switch to X" path: rule 3
                // exempt, freshness 1.0, novelty keyed on the APP. The launch
                // target is the class's most recently observed process name; a
                // class never observed since startup stays silent (nothing
                // honest to launch).
                let app_focus = consequent.resource_class.is_none()
                    && matches!(consequent.action.as_str(), "focus" | "open");
                let app_target = if app_focus {
                    match self.last_process.get(&consequent.app_class) {
                        Some(p) => Some(p.clone()),
                        None => continue,
                    }
                } else {
                    None
                };

                let (state, fresh, is_foreground, last_focused_ms) = if app_focus {
                    let app = consequent.app_class.as_str();
                    (
                        None,
                        1.0,
                        self.foreground_app.as_deref() == Some(app),
                        self.last_focused_app.get(app).copied(),
                    )
                } else {
                    // Rule 3 seam: a fresh, resumable connector state
                    // (doc 10 / M4; fakes in the M3 gate).
                    let state = (ctx.connector_lookup)(&consequent);
                    let fresh = state
                        .as_ref()
                        .map(|s| scorer::freshness(s, ctx.now_ms))
                        .unwrap_or(0.0);
                    let cons_res = consequent.resource_class.as_deref();
                    (
                        state,
                        fresh,
                        cons_res.is_some() && cons_res == self.foreground_resource.as_deref(),
                        cons_res.and_then(|r| self.last_focused.get(r).copied()),
                    )
                };

                let nov = if is_foreground {
                    0.0
                } else {
                    scorer::novelty(None, None, last_focused_ms, ctx.now_ms)
                };
                let score = scorer::score(conf, stats_now.dismiss_decay, fresh, nov);

                let input = TriggerInput {
                    score,
                    weighted_support: stats_now.weighted_support,
                    connector_state: state.as_ref(),
                    requires_fresh_state: !app_focus,
                    signature: &sig,
                    dismissal_step: row.mute.dismissal_step(ctx.now_ms),
                    consequent_is_foreground: is_foreground,
                    consequent_last_focused_ms: last_focused_ms,
                    now_ms: ctx.now_ms,
                };

                if self.gate.admit(&input, self.capture_on).is_ok() {
                    let connector_id = match app_target {
                        // "Switch to X": the sentinel ref the shell resolves to
                        // an app-focus dispatch (Path B analog, decision #15).
                        Some(process) => format!("{APP_FOCUS_REF_PREFIX}{process}"),
                        None => state.expect("rule 3 held").id.clone(),
                    };
                    out.push(SuggestionCandidate {
                        action_template: action_template_for(&consequent),
                        connector_id,
                        confidence: score,
                        pattern_id: row.pattern_id,
                    });
                    self.gate.note_emitted(&sig, ctx.now_ms);
                }
            }
        }

        // Temporal (time-of-day) candidates ride the same gate (decision #16).
        self.temporal_candidates(ctx, &mut out);

        // Overflow rule (§6.5): keep the highest-score candidates first (the
        // downstream queue drops lowest on overflow).
        out.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
        out
    }

    /// The temporal trigger path (owner decision #16, 2026-08-16 — doc 08 §4
    /// mined these but nothing ever fired them): when "now" falls in a formed
    /// pattern's peak time-of-day bucket, the predicted resource becomes a
    /// candidate through the SAME 7-rule gate as sequence patterns — score
    /// (`peak/total` confidence × decay × freshness × novelty), support floor
    /// (peak weighted mass), fresh connector state (rule 3 — temporal resources
    /// always have a resource class), per-signature cooldown, the hourly cap,
    /// novelty (the current event just stamped `last_focused`, so a return the
    /// user is making RIGHT NOW self-suppresses), and capture.
    ///
    /// Each fired pattern is backed by a real [`PatternRow`] keyed by
    /// [`temporal::signature_for`], so feedback (decay/mute ladder) and
    /// flush/hydrate persistence work exactly like sequence rows. The histogram
    /// itself is in-memory and re-mines after a restart; the row preserves what
    /// must survive — the decay and the mute.
    fn temporal_candidates(&mut self, ctx: &EngineContext<'_>, out: &mut Vec<SuggestionCandidate>) {
        let peaks: Vec<(String, usize, f64, f64)> = self
            .temporal
            .iter()
            .filter(|(_, h)| h.now_is_peak(ctx.now_ms))
            .filter_map(|(res, h)| {
                h.peak_bucket()
                    .map(|(bucket, peak)| (res.clone(), bucket, peak, h.total_mass()))
            })
            .collect();

        for (res, bucket, peak, total) in peaks {
            let consequent = temporal::consequent_token(&res);
            let sig = temporal::signature_for(bucket, &consequent);

            if !self.patterns.contains_key(&sig) {
                let id = self.next_local_id;
                self.next_local_id -= 1;
                self.patterns.insert(
                    sig.clone(),
                    PatternRow {
                        pattern_id: id,
                        stats: PatternStats::new(ctx.now_ms),
                        mute: MuteState::default(),
                        consequent: Some(consequent.clone()),
                        dirty: true,
                    },
                );
                self.id_index.insert(id, sig.clone());
            }
            let row = self.patterns.get_mut(&sig).expect("inserted above");
            // The histogram is the statistical truth for temporal patterns —
            // sync it into the row so the flush persists honest numbers and the
            // decay prune sees live support.
            row.stats.weighted_support = peak;
            row.stats.antecedent_total = total.max(peak);
            row.stats.last_updated_ms = ctx.now_ms;
            row.dirty = true;
            if row.mute.is_muted(ctx.now_ms) {
                continue; // muted signatures stay silent (doc 08 §7)
            }
            let conf = row.stats.confidence();
            let dismiss_decay = row.stats.dismiss_decay;
            let dismissal_step = row.mute.dismissal_step(ctx.now_ms);
            let pattern_id = row.pattern_id;

            // Rule 3: temporal bubbles resume state like any other bubble.
            let state = (ctx.connector_lookup)(&consequent);
            let fresh = state
                .as_ref()
                .map(|s| scorer::freshness(s, ctx.now_ms))
                .unwrap_or(0.0);
            let nov = scorer::novelty(
                Some(&res),
                self.foreground_resource.as_deref(),
                self.last_focused.get(&res).copied(),
                ctx.now_ms,
            );
            let score = scorer::score(conf, dismiss_decay, fresh, nov);

            let input = TriggerInput {
                score,
                weighted_support: peak,
                connector_state: state.as_ref(),
                requires_fresh_state: true,
                signature: &sig,
                dismissal_step,
                consequent_is_foreground: self.foreground_resource.as_deref()
                    == Some(res.as_str()),
                consequent_last_focused_ms: self.last_focused.get(&res).copied(),
                now_ms: ctx.now_ms,
            };
            if self.gate.admit(&input, self.capture_on).is_ok() {
                let state = state.expect("rule 3 held");
                out.push(SuggestionCandidate {
                    action_template: action_template_for(&consequent),
                    connector_id: state.id.clone(),
                    confidence: score,
                    pattern_id,
                });
                self.gate.note_emitted(&sig, ctx.now_ms);
            }
        }
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
            FeedbackEvent::Dismissed | FeedbackEvent::ThumbsDown | FeedbackEvent::Muted => {
                self.gate.adapt_cap(false)
            }
            FeedbackEvent::Expired => {}
        }
    }

    /// Maintenance hook (doc 08 §9): prune signatures with decayed weighted
    /// support below [`config::PRUNE_SUPPORT_FLOOR`]. Returns the pruned
    /// SIGNATURES so the caller can mirror the deletions to the `patterns`
    /// table — without the mirror they re-hydrate at the next restart
    /// (2026-08-15 review: this hook previously had no caller at all).
    ///
    /// **Sole `patterns` deleter** (owner decision #18, 2026-08-16): this decay
    /// prune — checked daily by the shell's pattern task, which mirrors the
    /// result to the DB — is the ONE owner of pattern-row deletion. The DB
    /// retention job's independent 180-day age prune was removed: decay strictly
    /// dominates it (a row untouched that long decayed under the floor weeks
    /// earlier), and a single owner means two timers can never disagree.
    pub fn prune(&mut self, now_ms: i64) -> Vec<String> {
        let mut flat: HashMap<String, (PatternStats, MuteState)> = self
            .patterns
            .iter()
            .map(|(k, v)| (k.clone(), (v.stats.clone(), v.mute.clone())))
            .collect();
        let doomed = feedback::prune_stale_patterns(
            &mut flat,
            now_ms,
            self.config.half_life_sequence_days,
        );
        for sig in &doomed {
            if let Some(row) = self.patterns.remove(sig) {
                self.id_index.remove(&row.pattern_id);
            }
            for sigs in self.antecedent_index.values_mut() {
                sigs.retain(|s| s != sig);
            }
        }
        doomed
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

    /// Seed the in-memory cache from persisted `patterns` rows at startup
    /// (CONN-M2, doc 08 §7). Without this the engine re-mines every signature cold
    /// after a restart — `dismiss_decay` resets to 1.0 and the mute is lost — so a
    /// dismissed or muted suggestion re-nags, and the next flush clobbers the saved
    /// ladder value. After hydration, re-observing a signature finds the existing
    /// row (`is_new == false` in [`on_event`](Self::on_event)), which preserves its
    /// decay + mute; feedback still routes by id (`id_index`); and a strong
    /// pre-restart pattern resumes firing (the `antecedent_index` is rebuilt so
    /// candidate generation reaches it).
    ///
    /// Rows load **clean** (`dirty = false`) — hydration is a load, not a change,
    /// so it never provokes a redundant flush. The persisted `support`/`confidence`
    /// are a lossy projection (support was rounded; the `⇒ *` denominator is only
    /// recoverable via confidence), so the weighted sums are reconstructed
    /// approximately — exact enough, since the engine re-credits on the next
    /// observation and what must be exact (decay + mute) is stored verbatim. A
    /// signature that fails to parse is skipped with a warning, never aborting the
    /// load. Intended to run once, before the event loop, on a fresh engine.
    ///
    /// Returns the SKIPPED (unparseable) signatures so the caller can delete
    /// those rows: they can never fire, take feedback, or decay — and since the
    /// engine's decay prune is now the sole `patterns` deleter (decision #18),
    /// a row the engine cannot cache would otherwise linger forever.
    pub fn hydrate(&mut self, rows: impl IntoIterator<Item = PersistedPattern>) -> Vec<String> {
        let mut skipped = Vec::new();
        for p in rows {
            let Some((ant_key, consequent)) = ngram::parse_signature(&p.signature) else {
                tracing::warn!(signature = %p.signature, "skipping unparseable persisted pattern");
                skipped.push(p.signature);
                continue;
            };
            let weighted_support = p.support as f64;
            // confidence = weighted_support / antecedent_total (capped at 1.0), so
            // antecedent_total = weighted_support / confidence (>= weighted_support).
            let antecedent_total = if p.confidence > 0.0 {
                (weighted_support / p.confidence).max(weighted_support)
            } else {
                weighted_support
            };
            let stats = PatternStats {
                weighted_support,
                antecedent_total,
                dismiss_decay: p.dismiss_decay,
                last_updated_ms: p.last_seen,
            };
            let mute = MuteState {
                recent_dismissals: p.recent_dismissals,
                muted_until: p.muted_until,
            };
            self.patterns.insert(
                p.signature.clone(),
                PatternRow {
                    pattern_id: p.pattern_id,
                    stats,
                    mute,
                    consequent: Some(consequent),
                    dirty: false,
                },
            );
            self.id_index.insert(p.pattern_id, p.signature.clone());
            let siblings = self.antecedent_index.entry(ant_key).or_default();
            if !siblings.contains(&p.signature) {
                siblings.push(p.signature);
            }
        }
        skipped
    }
}

impl Default for PatternEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Sentinel prefix on [`SuggestionCandidate::connector_id`] marking a
/// "switch to X" app-focus candidate (owner decision #15, 2026-08-16). The rest
/// of the id is the target's raw process name (e.g. `slack.exe`). The shell
/// resolves it into a synthetic `app_focus` connector-state row so the bubble
/// rides the normal suggestion pipeline and Path B click-resolve; real
/// connector ids are UUIDs, so the prefix cannot collide.
pub const APP_FOCUS_REF_PREFIX: &str = "app-focus:";

/// The launch-target process of an app-focus candidate's `connector_id`, if it
/// is one ([`APP_FOCUS_REF_PREFIX`]).
pub fn app_focus_target(connector_id: &str) -> Option<&str> {
    connector_id.strip_prefix(APP_FOCUS_REF_PREFIX)
}

/// Render the default action template for a consequent token (doc 08 §6 →
/// suggestion-generator). The generator expands `{title}`/`{position}` from the
/// connector's `reconstruct_payload` (doc 08 §6, doc 11 §3). A resource-less
/// consequent is an app-focus ("switch to X") candidate — `{app}` expands from
/// the synthetic `app_focus` state's payload (decision #15).
fn action_template_for(consequent: &Token) -> String {
    match consequent.resource_class.as_deref() {
        Some("youtube") => "Continue {title} — {position}".to_string(),
        Some(r) if r.starts_with("doc:") => "Reopen {title}".to_string(),
        Some(r) if r.starts_with("ide:") => "Back to {title}:{line}".to_string(),
        Some(r) if r.starts_with("url:") => "Return to {title}".to_string(),
        Some(_) => "Resume {title}".to_string(),
        None => "Switch to {app}".to_string(),
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
    fn no_fresh_connector_state_means_no_resume_bubble() {
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
        // Rule 3 still bars every RESUME candidate (resource-ful consequent, no
        // fresh state). Since decision #15, app-focus consequents may produce
        // "switch to X" candidates instead — those are the only kind allowed here.
        assert!(
            all.iter().all(|c| app_focus_target(&c.connector_id).is_some()),
            "no fresh resumable state ⇒ no resume bubble (rule 3); got {all:?}"
        );
    }

    /// Decision #15: a pure window-focus habit (ide → slack, no connector state
    /// anywhere) produces the lighter "switch to X" candidate through the same
    /// gate — including the not-recently-focused rule.
    #[test]
    fn pure_focus_pattern_yields_a_switch_to_candidate() {
        let mut engine = PatternEngine::new();
        engine.set_capture(true);
        let lookup = |_: &Token| -> Option<ConnectorState> { None };

        // 3 reps of (code focus → slack focus), spaced so slack's last focus is
        // stale (> 10 min) by each rep's code event.
        let mut ts = 0;
        for _ in 0..3 {
            ts += 12 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            engine.on_event(&focus_event(ts, "code.exe"), &ctx);
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            engine.on_event(&focus_event(ts, "slack.exe"), &ctx);
        }

        // 4th antecedent: support 3, slack last focused 12 min ago ⇒ novel.
        ts += 12 * MIN;
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
        let got = engine.on_event(&focus_event(ts, "code.exe"), &ctx);
        let c = got
            .iter()
            .find(|c| app_focus_target(&c.connector_id).is_some())
            .expect("switch-to candidate for the pure focus habit (#15)");
        assert_eq!(app_focus_target(&c.connector_id), Some("slack.exe"));
        assert_eq!(c.action_template, "Switch to {app}");
        assert!(c.confidence >= config::TAU_CONF, "same rule-1 threshold applies");

        // Not-recently-focused (rule 6) still gates it: past the 30 min
        // cooldown but with slack focused 5 min ago, recency must suppress.
        ts += 26 * MIN;
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
        engine.on_event(&focus_event(ts, "slack.exe"), &ctx);
        ts += 5 * MIN; // cooldown (31 min since shown) expired; focus is recent
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
        let got = engine.on_event(&focus_event(ts, "code.exe"), &ctx);
        assert!(
            got.iter().all(|c| app_focus_target(&c.connector_id) != Some("slack.exe")),
            "focused 5 min ago ⇒ not novel ⇒ no switch bubble (rule 6)"
        );
    }

    /// Decision #16: a formed time-of-day habit fires a bubble in its peak
    /// bucket via the normal trigger path, and its mute ladder works because
    /// the candidate is backed by a real pattern row.
    #[test]
    fn temporal_pattern_fires_in_its_peak_bucket() {
        std::env::set_var("APERTURE_TZ_OFFSET_MIN", "0");
        const DAY: i64 = 86_400_000;
        let mut engine = PatternEngine::new();
        engine.set_capture(true);
        let lookup = |tok: &Token| -> Option<ConnectorState> {
            (tok.resource_class.as_deref() == Some("youtube")).then(youtube_state)
        };
        let nine_am = 9 * 3_600_000;

        // One youtube return ~9am on 4 consecutive days — a single event per
        // day, so no n-gram can form: any candidate is temporal-only.
        for day in 0..4 {
            let ts = day * DAY + nine_am;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            let got = engine.on_event(
                &nav_event(ts, "chrome.exe", "https://youtube.com/watch?v=abc"),
                &ctx,
            );
            assert!(
                got.is_empty(),
                "the return being made right now must self-suppress (rule 6)"
            );
        }

        // Day 5, 9:05am, focused elsewhere: the predicted return window is now.
        let ts = 4 * DAY + nine_am + 5 * MIN;
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
        let got = engine.on_event(&focus_event(ts, "code.exe"), &ctx);
        let c = got.first().expect("temporal pattern fires in its peak bucket (#16)");
        assert_eq!(c.connector_id, "conn-yt", "rule 3: resumes fresh connector state");
        assert!(c.action_template.contains("{title}"));
        assert!(c.confidence >= config::TAU_CONF);
        let temporal_id = c.pattern_id;

        // Outside the peak bucket (9am + 6h) nothing temporal fires.
        let ts_off = 4 * DAY + nine_am + 6 * 3_600_000;
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts_off };
        let got = engine.on_event(&focus_event(ts_off, "code.exe"), &ctx);
        assert!(
            got.iter().all(|c| c.pattern_id != temporal_id),
            "no temporal bubble outside the predicted window"
        );

        // The feedback ladder reaches temporal rows: 3 dismissals mute it.
        engine.apply_feedback(temporal_id, FeedbackEvent::Dismissed, ts_off);
        engine.apply_feedback(temporal_id, FeedbackEvent::Dismissed, ts_off + MIN);
        engine.apply_feedback(temporal_id, FeedbackEvent::Dismissed, ts_off + 2 * MIN);
        let ts_next = 5 * DAY + nine_am + 5 * MIN; // next day's peak window
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts_next };
        let got = engine.on_event(&focus_event(ts_next, "code.exe"), &ctx);
        assert!(
            got.iter().all(|c| c.pattern_id != temporal_id),
            "a muted temporal pattern stays silent in its window (doc 08 §7)"
        );
    }

    /// Decision #17: the settings-loaded config actually moves the gate.
    #[test]
    fn set_config_changes_trigger_behavior() {
        let mut engine = PatternEngine::new();
        engine.set_capture(true);
        engine.set_config(config::EngineConfig {
            cold_start_support_floor: 10.0, // far above the default 3
            ..config::EngineConfig::default()
        });
        let lookup = |tok: &Token| -> Option<ConnectorState> {
            (tok.resource_class.as_deref() == Some("youtube")).then(youtube_state)
        };
        let mut ts = 0;
        let mut all = Vec::new();
        for _ in 0..4 {
            ts += 12 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            all.extend(engine.on_event(&focus_event(ts, "code.exe"), &ctx));
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            all.extend(engine.on_event(
                &nav_event(ts, "chrome.exe", "https://youtube.com/watch?v=a"),
                &ctx,
            ));
            ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: ts };
            all.extend(engine.on_event(&focus_event(ts, "slack.exe"), &ctx));
        }
        assert!(
            all.is_empty(),
            "a configured support floor of 10 suppresses what fires at the default (#17)"
        );
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

    /// CONN-M2: the decay/mute ladder must survive a restart. Mine + mute a
    /// pattern, snapshot what a flush would persist (incl. the migration-0002 mute
    /// columns), hydrate a *fresh* engine, and re-run the same script — the muted
    /// signature must stay silent. Without hydrate the engine re-mines cold
    /// (`dismiss_decay = 1.0`, mute lost) and re-nags.
    #[test]
    fn hydrate_keeps_a_dismissed_pattern_muted_across_restart() {
        let lookup = |tok: &Token| -> Option<ConnectorState> {
            (tok.resource_class.as_deref() == Some("youtube")).then(youtube_state)
        };
        // One SC2 repetition (ide focus → youtube nav → slack focus).
        let rep = |engine: &mut PatternEngine, ts: &mut i64| -> Vec<SuggestionCandidate> {
            *ts += 12 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: *ts };
            let got = engine.on_event(&focus_event(*ts, "code.exe"), &ctx);
            *ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: *ts };
            engine.on_event(&nav_event(*ts, "chrome.exe", "https://youtube.com/watch?v=a"), &ctx);
            *ts += 2 * MIN;
            let ctx = EngineContext { connector_lookup: &lookup, now_ms: *ts };
            engine.on_event(&focus_event(*ts, "slack.exe"), &ctx);
            got
        };

        // Pre-restart: mine until it fires, then dismiss 3× to mute it.
        let mut a = PatternEngine::new();
        a.set_capture(true);
        let mut id = None;
        let mut ts = 0;
        for _ in 0..4 {
            if let Some(c) = rep(&mut a, &mut ts).first() {
                id = Some(c.pattern_id);
            }
        }
        let id = id.expect("pattern fired at least once (support 3)");
        a.apply_feedback(id, FeedbackEvent::Dismissed, ts);
        a.apply_feedback(id, FeedbackEvent::Dismissed, ts + MIN);
        a.apply_feedback(id, FeedbackEvent::Dismissed, ts + 2 * MIN);

        // Snapshot exactly what `flush_patterns` persists — including the mute
        // columns that migration 0002 adds.
        let persisted: Vec<PersistedPattern> = a
            .dirty_rows()
            .into_iter()
            .map(|(sig, row)| PersistedPattern {
                pattern_id: row.pattern_id,
                signature: sig.to_string(),
                support: row.stats.weighted_support.round() as i64,
                confidence: row.stats.confidence(),
                last_seen: row.stats.last_updated_ms,
                dismiss_decay: row.stats.dismiss_decay,
                muted_until: row.mute.muted_until,
                recent_dismissals: row.mute.recent_dismissals.clone(),
            })
            .collect();
        let muted_id = persisted
            .iter()
            .find(|p| p.muted_until.is_some())
            .map(|p| p.pattern_id)
            .expect("the thrice-dismissed pattern is muted before the restart");

        // Post-restart: hydrate a fresh engine and re-run the same script. The
        // muted signature (mute expires 7 d out, well past this run) never fires.
        let mut b = PatternEngine::new();
        b.set_capture(true);
        b.hydrate(persisted);
        let mut ts2 = 0;
        let mut fired = Vec::new();
        for _ in 0..4 {
            fired.extend(rep(&mut b, &mut ts2));
        }
        assert!(
            fired.iter().all(|c| c.pattern_id != muted_id),
            "CONN-M2: a muted pattern must not re-nag after a restart+hydrate"
        );
    }

    /// CONN-M2 (the other half): re-mining a hydrated signature must NOT reset its
    /// `dismiss_decay` back to the cold 1.0 — otherwise the very next flush would
    /// clobber the persisted ladder value.
    #[test]
    fn hydrate_preserves_dismiss_decay_through_re_mining() {
        let sig = "ide:focus:∅ ⇒ browser:navigation:youtube".to_string();
        let mut engine = PatternEngine::new();
        engine.set_capture(true);
        engine.hydrate([PersistedPattern {
            pattern_id: 7,
            signature: sig.clone(),
            support: 3,
            confidence: 1.0,
            last_seen: 0,
            dismiss_decay: 0.288, // ~three dismissals' worth (ADR-033 ladder)
            muted_until: None,
            recent_dismissals: vec![],
        }]);
        // Re-observe (ide focus → youtube nav): the gram closes onto the hydrated
        // signature, so `is_new` is false and `credit_occurrence` runs WITHOUT
        // touching `dismiss_decay`.
        let lookup = |_: &Token| -> Option<ConnectorState> { Some(youtube_state()) };
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: 10 * MIN };
        engine.on_event(&focus_event(10 * MIN, "code.exe"), &ctx);
        let ctx = EngineContext { connector_lookup: &lookup, now_ms: 12 * MIN };
        engine.on_event(&nav_event(12 * MIN, "chrome.exe", "https://youtube.com/watch?v=z"), &ctx);

        let decay = engine
            .dirty_rows()
            .into_iter()
            .find(|(s, _)| *s == sig)
            .map(|(_, r)| r.stats.dismiss_decay)
            .expect("the re-mined signature is present as a dirty row");
        assert!(
            (decay - 0.288).abs() < 1e-9,
            "re-mining a hydrated signature keeps its decay (CONN-M2), got {decay}"
        );
    }
}
