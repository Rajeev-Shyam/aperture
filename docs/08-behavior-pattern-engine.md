# Doc 08 — Behavior & Pattern Engine

## 1. Interface
| | |
|---|---|
| **Inputs** | Event stream (bus + SQLite), embeddings (`ctx_vec`), fresh `connector_state`, suggestion feedback events |
| **Outputs** | `SuggestionCandidate{action_template, connector_id, confidence, pattern_id}` → Suggestion Generator → Bubble UI. Optional VLM disambiguation requests (Doc 06 §4a). **Never a cloud call** (locked answer A) |
| **Resource cost** | CPU-only, incremental; O(recent-window) per event; negligible RAM beyond pattern table cache |

## 2. Event normalization → tokens
Each event becomes a token `(app_class, action, resource_class)`:
- `app_class`: process mapped through a small alias table (chrome/edge/firefox→`browser`, code→`ide`, excel/winword→`office`, else process name).
- `action`: the event type (focus/open/navigation/media/document/ide).
- `resource_class`: from the connector type when present (`youtube`, `doc:xlsx`, `ide:rs`, `url:domain`), else `∅`.
Example: opening a tutorial video ⇒ `(browser, navigation, youtube)`.

## 3. Sessionization
A new `session_id` starts when the idle gap exceeds a **rolling idle-gap threshold** derived (forward-applied, never retro-sessionizing) from the user's own inter-event gap distribution, falling back to a **15 min cold-start default** [ASSUMPTION] until enough history accrues. Sessions bound n-gram extraction so overnight gaps don't fabricate sequences.

## 4. Signatures & statistics
- Extract sliding **n-grams (n = 2..4)** of tokens within a session. The trailing token is the *consequent*; the prefix is the *antecedent*.
- `signature = join(antecedent) ⇒ consequent` stored in `patterns`.
- **Support:** count of occurrences, recency-weighted: each occurrence contributes `w = 0.5^(age_days/H)`, with the exponential half-life split by pattern type — **H = ~14 d for sequence patterns / ~5 d for temporal patterns** [ASSUMPTION] (workflows are stable; time-of-day habits shift faster).
- **Confidence:** `conf = W(antecedent ⇒ consequent) / W(antecedent ⇒ *)` — a recency-weighted conditional probability.
- **Temporal patterns:** independently, per-resource return-visit periodicity (time-of-day bucket histogram, 2-hour buckets). A resource with ≥3 weighted returns in the same bucket forms a `temporal` pattern (e.g., "opens the budget sheet ~9am").

## 5. Candidate generation & scoring
On each new event, match the current token tail against pattern antecedents (exact match on n-grams; plus a semantic assist: cosine similarity of the current context embedding to the pattern's stored centroid ≥ 0.75 may substitute for one token [ASSUMPTION — evaluate at M3]).
```
score = conf                       // §4
      × dismiss_decay              // §7 feedback
      × freshness(connector_state) // 1.0 if within TTL, else 0 → no candidate
      × novelty                    // 0 if the resource is foreground OR was focused in the last ~10 min (never suggest what's on screen or just-seen)
```

## 6. Proactive trigger rule (all must hold)
1. `score ≥ τ_conf = 0.7` [VERIFY — tuned against SC7 at M3]
2. Weighted support ≥ **3** (cold-start floor [ASSUMPTION])
3. A **fresh, resumable** `connector_state` exists for the consequent (Doc 10 TTLs)
4. Cooldown: same signature not shown in the last **30 min** [ASSUMPTION]
5. Global cap: **adaptive 2→8 suggestions/hour** (click-through-driven — starts at the 2/hr floor, opens toward the 8/hr ceiling as suggestions earn clicks, closes when ignored); queue overflow drops lowest score
6. The consequent's resource is not currently foreground **and was not focused in the last ~10 min** (novelty)
7. Capture is ON
When all hold ⇒ emit the candidate; the Suggestion Generator renders `action_template` ("Continue {title} — {position}") into a `BubbleSpec`.

## 7. Feedback loop
| Signal | Effect |
|---|---|
| `suggestion_clicked` | `dismiss_decay ← min(1.0, decay × 1.25)`; support reinforced |
| `suggestion_dismissed` | Escalating, gently: **1st dismiss** ⇒ cooldown ×2 + `dismiss_decay ← decay × 0.8`; **2nd** ⇒ cooldown ×4 + `dismiss_decay ← decay × 0.6`; **3rd** ⇒ signature muted [ASSUMPTION] |
| `suggestion_expired` (ignored) | mild: `decay × 0.9` |
| `suggestion_useful` (explicit **"useful?" thumbs**, Q81) | thumbs-**up** ≈ a strong click (reinforces support + nudges `dismiss_decay` up like a click); thumbs-**down** ≈ a dismiss-with-signal (feeds the escalating dismissal ladder above) |
These write back to `patterns` and are the lever that meets SC7 without cloud help.

## 8. VLM assist (optional, local)
When a high-support antecedent matches but `resource_class = ∅` (OCR couldn't classify the consequent), the engine may file a Doc 06 wake request to classify the scene. The candidate is **not** delayed for it (Doc 02 Path A invariant).

## 9. Failure modes
| Failure | Behavior |
|---|---|
| Cold start (new install) | No suggestions until floors are met — silence beats noise |
| Over-triggering | Caps/cooldowns + decay (§6–7); tunables exposed in settings |
| Concept drift (habits change) | Half-life weighting ages old patterns out in ~3 weeks |
| Pattern table bloat | Prune signatures with weighted support < 0.5 weekly |
| Clock changes / DST | Temporal buckets keyed to local wall-clock by design |

---
> **R2 amendments applied** (see docs/19–21): ADR-032 (rolling session gap, adaptive 2→8/hr cap), ADR-033 (τ_conf 0.7, split half-lives, extended novelty, softened dismissal curve), Q81 ("useful?" thumbs feedback).
